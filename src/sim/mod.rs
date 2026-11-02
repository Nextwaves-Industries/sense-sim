//! Simulation in Sense Sim: the Create Simulation dialog, the studies, their jobs and results. The
//! physics lives in `sim-core`; this is the app around it.

pub mod autotest;
pub mod dialog;
pub mod notices;
pub mod panel;
pub mod persist;
pub mod scene_body;
pub mod tools;
pub mod view;

use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass};
use sim_core::jobs::JobManager;
use sim_core::study::Study;

use crate::design::{Slot, Workspace};

#[derive(Resource)]
pub struct SimState {
    pub panel_open: bool,
    pub dialog: Option<dialog::Dialog>,
    pub studies: Vec<Study>,
    pub active: Option<usize>,
    pub jobs: JobManager,
    pub view: Option<view::View>,
    /// A condition waiting for faces from the design workspace: (study, condition).
    pub picking: Option<(usize, usize)>,
    /// The same, from an object in the 3D scene.
    pub scene_pick: Option<(usize, usize)>,
    /// Scene objects, as bodies.
    pub scene: scene_body::SceneBodies,
    /// The pointer is on the UI, so a click isn't for the scene.
    pub pointer_over_ui: bool,
    pub installed: dialog::Installed,
    pub message: Option<String>,
    /// Job whose log is shown.
    pub log_job: Option<u64>,
    pub notices_open: bool,
}

impl Default for SimState {
    fn default() -> Self {
        let root = std::env::var_os("LOCALAPPDATA").map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from).join("SenseSim").join("sim");
        Self {
            panel_open: false,
            dialog: None,
            studies: Vec::new(),
            active: None,
            jobs: JobManager::new(root),
            view: None,
            picking: None,
            scene_pick: None,
            scene: Default::default(),
            pointer_over_ui: false,
            installed: installed_now(),
            message: None,
            log_job: None,
            notices_open: false,
        }
    }
}

/// What's installed right now. Asking WSL takes a moment, so this runs on start and after installs.
pub fn installed_now() -> dialog::Installed {
    let wsl_available = sim_core::jobs::wsl::available();
    dialog::Installed { native_fea: sim_core::tools::gmsh().is_some() && sim_core::tools::ccx().is_some(), wsl_available, solvers_distro: wsl_available && sim_core::provision::wsl_ready() }
}

pub struct SimPlugin;

impl Plugin for SimPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SimState>()
            .init_resource::<view::ViewAssets>()
            .init_resource::<persist::Hook>()
            .add_systems(
                Update,
                (
                    view::sync,
                    scene_body::sync,
                    scene_body::pick.after(scene_body::sync),
                    scene_body::step_aside,
                    // Right before the scene acts on them: anything that adds actions in this schedule
                    // has to come first, or the scene would act on one this never saw.
                    persist::peek_actions.after(crate::scene::keyboard_shortcuts).before(crate::scene::apply_actions),
                    persist::after_actions.after(crate::scene::apply_actions),
                ),
            )
            .add_systems(EguiPrimaryContextPass, ui.after(crate::ui::ui_system).before(crate::ime::sync_ime))
            .add_systems(Last, stop_jobs_on_exit);
    }
}

#[allow(clippy::too_many_arguments)]
fn ui(mut contexts: EguiContexts, mut state: ResMut<SimState>, mut slot: NonSendMut<Slot>, mut workspace: ResMut<Workspace>, mut panel_slot: ResMut<crate::ui::LeftPanelSlot>, mut block: ResMut<crate::scene::UiBlock>) -> Result {
    panel_slot.results_shown = state.view.is_some();
    if std::mem::take(&mut panel_slot.open_simulations) {
        state.panel_open = true;
    }
    panel::finish_pick(&mut state, &mut slot);
    if std::mem::take(&mut slot.want_notices) {
        state.notices_open = true;
    }
    state.pointer_over_ui = block.pointer;
    // Clicks are for choosing faces now. Right-drag still turns the view.
    if state.scene_pick.is_some() {
        block.pointer = true;
    }
    let ctx = contexts.ctx_mut()?.clone();
    if state.notices_open {
        let mut open = true;
        notices::show(&ctx, &mut open);
        state.notices_open = open;
    }
    if *workspace != Workspace::Scene || panel_slot.bench.current != crate::ui::Workbench::Simulation {
        return Ok(());
    }
    if state.panel_open {
        panel::show(&ctx, &mut state, &mut slot, &mut workspace);
    }
    let installed = state.installed;
    if let Some(mut d) = state.dialog.take() {
        let mut open = true;
        if let Some(analysis) = dialog::show(&ctx, &mut d, installed, &mut open) {
            let n = state.studies.len() + 1;
            let label = sim_core::catalog::analysis(analysis).label;
            state.studies.push(Study::new(format!("{label} {n}"), analysis));
            state.active = Some(state.studies.len() - 1);
            state.panel_open = true;
        }
        if open {
            state.dialog = Some(d);
        }
    }
    Ok(())
}

fn stop_jobs_on_exit(exits: MessageReader<AppExit>, state: Res<SimState>) {
    if !exits.is_empty() {
        state.jobs.cancel_all();
    }
}
