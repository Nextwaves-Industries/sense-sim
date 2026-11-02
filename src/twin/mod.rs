//! The Factory workbench: real hardware found on the network, shown in the scene.
//!
//! The viewer only mirrors what the Sense Twin service knows and reads it over a named pipe.
//! It can't scan a network or command a device, and that's by construction: the code for
//! those lives in the service and the operator console, and a test in `twin_proto` fails the
//! build if this app ever links the command crate. The AI assistant and MCP tools run inside
//! this process, so that boundary is what keeps them away from real machines.
//!
//! What a device *is* comes from the service. Where it *stands* belongs to the layout, in
//! [`TwinScene`], because a scan can't tell.

mod lines;
mod persist;
mod source;
mod ui;
mod view;
mod yard;

use std::collections::BTreeMap;

use bevy::prelude::*;
use bevy_egui::egui;
use twin_proto::inventory::Mirror;
use twin_proto::model::{Device, DeviceClass, DeviceId, Liveness};

pub use source::LinkStatus;
use source::{Command, Link, Update};
use view::TwinMarker;
use yard::Yard;

use crate::theme;
use crate::ui::{LeftPanelSlot, Workbench};

/// Sizes offered for the demo factory.
pub const DEMO_SIZES: [usize; 3] = [60, 500, 5000];

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum TwinPage {
    Lines,
    /// Where the workbench opens: a real plant has devices long before anyone defines its lines.
    #[default]
    Devices,
    Events,
    Connection,
}

pub enum TwinRequest {
    /// Show this many invented devices, or `None` to go back to the service.
    Demo(Option<usize>),
    /// Move the camera to where the devices are.
    FrameDevices,
}

/// What the ribbon, the project tree and the Factory card tell each other.
pub struct TwinSlot {
    pub open: bool,
    pub page: TwinPage,
    pub rect: Option<egui::Rect>,
    pub labels: bool,
    pub requests: Vec<TwinRequest>,
    pub status: LinkStatus,
    pub total: usize,
    pub down: usize,
    pub suspect: usize,
    pub stale: usize,
    pub events: usize,
    pub lines: usize,
    pub lines_health: theme::Dot,
}

impl Default for TwinSlot {
    fn default() -> Self {
        Self { open: true, page: TwinPage::default(), rect: None, labels: true, requests: Vec::new(), status: LinkStatus::default(), total: 0, down: 0, suspect: 0, stale: 0, events: 0, lines: 0, lines_health: theme::Dot::Optional }
    }
}

impl TwinSlot {
    pub fn is_demo(&self) -> bool {
        self.status == LinkStatus::Demo
    }

    /// The plant at a glance. Green only if every device is known to be up: a device nobody
    /// can see right now is not a healthy one.
    pub fn health(&self) -> theme::Dot {
        match (self.total, self.down, self.suspect, self.stale) {
            (0, ..) => theme::Dot::Optional,
            (_, 1.., _, _) => theme::Dot::Todo,
            (_, _, 1.., _) => theme::Dot::Busy,
            (_, _, _, 1..) => theme::Dot::Unknown,
            _ => theme::Dot::Done,
        }
    }

    /// Opens this page, or closes the card if it's already showing.
    pub fn toggle_page(&mut self, page: TwinPage) {
        let showing = self.open && self.page == page;
        self.open = !showing;
        self.page = page;
    }
}

/// A device's position, set by hand. Part of the layout, not of the inventory.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub position: [f32; 3],
}

/// Per scene, so each tab can arrange the same plant its own way.
#[derive(Resource, Default)]
pub struct TwinScene {
    pub placed: BTreeMap<DeviceId, Placement>,
}

#[derive(Resource, Default)]
pub struct TwinSelection {
    pub device: Option<DeviceId>,
    pub hovered: Option<DeviceId>,
}

#[derive(Default, Clone)]
pub struct Counts {
    pub up: usize,
    pub suspect: usize,
    pub down: usize,
    pub stale: usize,
    pub by_class: BTreeMap<DeviceClass, usize>,
    pub by_vendor: BTreeMap<String, usize>,
}

/// The whole plant, the same in every tab.
#[derive(Resource, Default)]
pub struct TwinInventory {
    pub mirror: Mirror,
    pub status: LinkStatus,
    /// Goes up whenever the device list or the connection changes.
    pub generation: u64,
    pub counts: Counts,
    pub lines: lines::LineTally,
}

impl TwinInventory {
    /// The data is invented, whichever side invented it: the service can serve a demo too,
    /// and with no warning that would look exactly like a real plant.
    pub fn showing_demo(&self) -> bool {
        self.status == LinkStatus::Demo || (self.mirror.demo && !self.mirror.devices.is_empty())
    }

    /// With the service gone, what's on screen is only what was last known.
    pub fn shown_liveness(&self, device: &Device) -> Liveness {
        match (&self.status, device.liveness) {
            (_, Liveness::Retired) => Liveness::Retired,
            (LinkStatus::Lost(_), _) => Liveness::Stale,
            (_, liveness) => liveness,
        }
    }

    fn recount(&mut self) {
        let mut counts = Counts::default();
        for device in self.mirror.devices.values().filter(|d| d.liveness != Liveness::Retired) {
            match self.shown_liveness(device) {
                Liveness::Up => counts.up += 1,
                Liveness::Suspect => counts.suspect += 1,
                Liveness::Down => counts.down += 1,
                _ => counts.stale += 1,
            }
            *counts.by_class.entry(device.class).or_default() += 1;
            let vendor = device.identity.vendor.as_deref().unwrap_or("Unknown vendor");
            *counts.by_vendor.entry(vendor.to_string()).or_default() += 1;
        }
        self.counts = counts;
        self.lines = lines::LineTally::of(self);
    }
}

/// Starts the link the first time the Factory workbench opens, so people who never use it
/// don't get a thread polling a pipe. Then brings its news into the app.
#[allow(clippy::too_many_arguments)]
fn pump_link(
    mut link: Local<Option<Link>>,
    mut inventory: ResMut<TwinInventory>,
    mut slot: ResMut<LeftPanelSlot>,
    mut yard: ResMut<Yard>,
    mut selection: ResMut<TwinSelection>,
) {
    let wanted = slot.bench.current == Workbench::Factory || !slot.twin.requests.is_empty();
    if link.is_none() && wanted {
        *link = Some(Link::start());
    }
    let requests = std::mem::take(&mut slot.twin.requests);
    for request in requests {
        match request {
            TwinRequest::Demo(count) => {
                if let Some(link) = &*link {
                    let _ = link.commands.send(Command::Demo(count));
                }
            }
            TwinRequest::FrameDevices => yard.frame_pending = true,
        }
    }

    if let Some(link) = &*link {
        let mut changed = false;
        while let Ok(update) = link.updates.try_recv() {
            changed = true;
            match update {
                Update::Status(status) => inventory.status = status,
                Update::Sync(reply) => inventory.mirror.apply(reply),
                Update::Clear => inventory.mirror = Mirror::default(),
            }
        }
        if changed {
            inventory.generation += 1;
            inventory.recount();
            if selection.device.as_ref().is_some_and(|id| !inventory.mirror.devices.contains_key(id)) {
                selection.device = None;
            }
        }
    }

    let counts = &inventory.counts;
    let twin = &mut slot.twin;
    twin.status = inventory.status.clone();
    twin.total = counts.up + counts.suspect + counts.down + counts.stale;
    twin.down = counts.down;
    twin.suspect = counts.suspect;
    twin.stale = counts.stale;
    twin.events = inventory.mirror.events.len();
    twin.lines = inventory.lines.total;
    twin.lines_health = inventory.lines.health();
}

pub struct TwinPlugin;

impl Plugin for TwinPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TwinInventory>()
            .init_resource::<TwinScene>()
            .init_resource::<TwinSelection>()
            .init_resource::<Yard>()
            .init_resource::<persist::LayoutHook>()
            .add_systems(Startup, view::setup_assets)
            .add_systems(
                Update,
                (
                    (pump_link, view::sync_markers).chain(),
                    view::frame_request.before(crate::scene::apply_actions),
                    view::frame_devices.after(crate::scene::apply_actions),
                    view::marker_pointer.before(crate::scene::interact).after(crate::scene::orbit_camera),
                    view::sync_selection.after(crate::scene::interact),
                    persist::peek_actions.before(crate::scene::apply_actions),
                    persist::after_actions.after(crate::scene::apply_actions),
                ),
            )
            .add_systems(PostUpdate, view::draw.after(TransformSystems::Propagate))
            .add_systems(bevy_egui::EguiPrimaryContextPass, ui::twin_ui.after(crate::ui::ui_system).before(crate::ime::sync_ime));
    }
}
