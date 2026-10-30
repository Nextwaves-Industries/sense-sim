//! Connected systems: ports on assets, links between them (pipes, shafts,
//! belts, conveyor transfers), generated piping and machines, pipe routing,
//! and a running plant (drives, joint motion, conveyors, hydraulics) that
//! the assistant builds with the `plant_*` tools.

pub mod connect;
pub mod parts;
pub mod persist;
pub mod ports;
pub mod recipes;
pub mod route;
pub mod sim;
pub mod tools;
pub mod ui;
pub mod view;

use bevy::prelude::*;

pub use connect::Links;
pub use sim::Plant;

/// `--plant-test layout.json --duration N --report out.json`: loads the
/// layout, waits for every asset, fast-forwards the plant and writes the report.
#[derive(Resource, Clone)]
pub struct AutoTest {
    pub duration: f32,
    pub report: std::path::PathBuf,
    pub frames_ready: u32,
}

pub struct PlantPlugin {
    pub autotest: Option<AutoTest>,
}

impl Plugin for PlantPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Links>()
            .init_resource::<Plant>()
            .init_resource::<persist::Hook>()
            .init_resource::<view::Liquids>()
            .init_gizmo_group::<view::FlowGizmos>()
            .add_systems(Startup, view::setup)
            .add_systems(
                Update,
                (
                    persist::peek_actions.before(crate::scene::apply_actions),
                    persist::after_actions.after(crate::scene::apply_actions).after(crate::rtls::persist::after_actions),
                    persist::track_names.after(crate::scene::apply_actions),
                    sim::mark_floating.after(crate::scene::process_pending),
                    sim::step.after(crate::scene::apply_actions).before(crate::scene::apply_joints).before(crate::undo::undo_system),
                    sim::keep_on_belts.after(sim::step).after(crate::scene::gravity),
                    view::sync_liquid.after(sim::step),
                ),
            )
            .add_systems(PostUpdate, view::draw.after(TransformSystems::Propagate))
            .add_systems(bevy_egui::EguiPrimaryContextPass, ui::plant_ui.after(crate::ui::ui_system));
        if let Some(t) = &self.autotest {
            app.insert_resource(t.clone()).add_systems(Update, autotest.after(sim::step));
        }
    }
}

fn autotest(world: &mut World) {
    let loading = world.resource::<crate::prototype::PrototypeCache>().loading_count() > 0 || !world.resource::<crate::scene::PendingSpawns>().0.is_empty();
    let elapsed = world.resource::<Time>().elapsed_secs();
    let mut t = world.resource::<AutoTest>().clone();
    if loading || elapsed < 3.0 {
        t.frames_ready = 0;
        world.insert_resource(t);
        return;
    }
    t.frames_ready += 1;
    world.insert_resource(t.clone());
    if t.frames_ready != 10 {
        return;
    }
    let model = sim::Model::from_world(world);
    let links = world.resource::<Links>().clone();
    let plant = world.resource::<Plant>();
    let mut report = sim::simulate(&model, &links, plant, t.duration);
    report["checks"] = serde_json::json!(tools::check_issues(world));
    let text = serde_json::to_string_pretty(&report).unwrap_or_default();
    if let Err(e) = std::fs::write(&t.report, &text) {
        eprintln!("plant test: cannot write {}: {e}", t.report.display());
    }
    println!("{text}");
    world.write_message(AppExit::Success);
}
