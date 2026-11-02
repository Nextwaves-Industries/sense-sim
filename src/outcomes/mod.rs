//! Predictions a person can commit to. Every recipe here runs the same loop from
//! the `proof` crate: predict with a range, measure, calibrate, predict again.
//!
//! - [`survey`]: fit the radio model of a site to a short walk with a real tag.
//! - [`stress`]: the site on its bad days: full racks, crowds, a dead anchor.
//! - [`drift`]: spot when a live site stops matching its model, and why.
//! - [`spec`]: which material choices decide coverage in a building not yet built.
//! - [`hardware`]: the antenna and device recipes as background jobs.
//! - [`erces`]: can firefighters' radios reach every floor of a building not built yet?
//! - [`compare`]: which technology meets the target here, and what it costs to own.
//! - [`acceptance`]: the points to test a handover on, and the signed-off result.
//! - [`anchors`]: where the anchors really are, from the ranges between them.
//! - [`battery`]: how fast a tag can report and still last.
//! - [`esl`]: can every shelf label be reached, and how long a price change takes.
//! - [`route`]: the radio a robot sees along its route.
//! - [`proximity`]: forklift alerts tested at the real corners.

pub mod acceptance;
pub mod anchors;
pub mod battery;
pub mod compare;
pub mod drift;
pub mod erces;
pub mod esl;
pub mod hardware;
pub mod proximity;
pub mod route;
pub mod spec;
pub mod stress;
pub mod survey;
pub mod tools;
pub mod ui;

use std::path::PathBuf;

use bevy::prelude::*;
use proof::{Claim, Ledger};

/// Every prediction made so far and how it held up, kept between sessions.
#[derive(Resource, Default)]
pub struct Predictions {
    pub ledger: Ledger,
}

impl Predictions {
    fn path() -> PathBuf {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
        PathBuf::from(base).join("SenseSim").join("predictions.json")
    }

    pub fn load() -> Self {
        let path = Self::path();
        let Ok(text) = std::fs::read_to_string(&path) else { return Self::default() };
        match serde_json::from_str::<Ledger>(&text) {
            Ok(mut ledger) => {
                ledger.reassess();
                Self { ledger }
            }
            Err(e) => {
                // Don't let the next save overwrite a record we couldn't read.
                let kept = path.with_extension("unreadable.json");
                warn!("{} can't be read ({e}); keeping it as {}", path.display(), kept.display());
                let _ = std::fs::rename(&path, &kept);
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        if let Err(e) = self.ledger.save_to(&Self::path()) {
            warn!("the predictions ledger wasn't saved: {e}");
        }
    }

    pub fn put(&mut self, c: Claim) {
        self.ledger.put(c);
        self.save();
    }
}

/// What's known about one scene doesn't carry over to the next. A layout's own stated
/// materials come back with it (`rtls::persist`).
fn scene_changes(actions: Res<crate::scene::Actions>, mut drift: ResMut<drift::DriftState>) {
    use crate::rtls::radio::{SiteConditions, clear_object_materials};
    use crate::scene::Action;
    let (clear, load) = actions.0.iter().fold((false, false), |(c, l), a| (c || matches!(a, Action::ClearScene), l || matches!(a, Action::LoadLayout(_))));
    if clear {
        clear_object_materials();
    }
    if clear || load {
        SiteConditions::set(SiteConditions::default());
        // FDTD results too: they were runs of the scene that just went.
        crate::rtls::radio::MaterialCal::clear_bands();
        drift.baseline.clear();
        drift.last = None;
    }
}

pub struct OutcomesPlugin;

impl Plugin for OutcomesPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Predictions::load())
            .init_resource::<hardware::Jobs>()
            .init_resource::<survey::SurveyState>()
            .init_resource::<drift::DriftState>()
            .init_resource::<acceptance::AcceptanceState>()
            .init_resource::<ui::OutcomesUi>()
            .add_systems(Update, (hardware::collect_finished, ui::run_pending, scene_changes.before(crate::scene::apply_actions)));
    }
}
