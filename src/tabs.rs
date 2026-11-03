//! Several scenes open at once, one per tab.
//!
//! The rest of the app knows about one scene: the entities with `PlacedObject`,
//! `Device` or `Zone`, and a set of resources. So a tab that is not showing is
//! *parked*, which keeps switching cheap and leaves every existing system alone:
//!
//! - Its roots keep their entities, hidden, with the marker component moved into a
//!   `Parked*` wrapper, so queries simply stop seeing them. Nothing is reloaded or
//!   respawned on the way back, and selections, tracks and joint poses stay valid.
//! - Its mesh parts lose `PartBvh`, because radio and picking queries read parts
//!   without looking at the root: a hidden wall must not block a visible signal.
//! - Its per-scene resources move into the tab's [`Stash`].
//!
//! A parked tab does not simulate, so it costs next to nothing per frame. A layout
//! is only read the first time its tab is shown.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;

use crate::prototype::{ProtoState, Prototype, PrototypeCache};
use crate::rtls::device::{Device, Zone};
use crate::scene::{Action, Actions, OrbitCamera, PartBvh, PartOf, PartRest, PendingDownloads, PendingSpawns, PlacedObject, Status};
use crate::rtls::ui::RtlsUiState;
use crate::ui::{LeftPanelSlot, ProjectView, UiState};
use crate::undo::UndoStack;

/// Roots freed per frame after closing a tab, so closing a big scene doesn't hitch.
const DESPAWN_PER_FRAME: usize = 32;

#[derive(Component)]
pub struct ParkedObject {
    owner: u64,
    object: PlacedObject,
    visibility: Visibility,
}

#[derive(Component)]
pub struct ParkedDevice {
    owner: u64,
    device: Device,
    visibility: Option<Visibility>,
}

#[derive(Component)]
pub struct ParkedZone {
    owner: u64,
    zone: Zone,
}

/// Declares the resources that belong to one scene, and how to exchange them.
macro_rules! per_scene {
    ($($field:ident: $ty:ty),* $(,)?) => {
        /// A parked tab's resources. The showing tab's stash holds blanks.
        #[derive(Default)]
        struct Stash {
            $($field: $ty,)*
        }

        impl Stash {
            fn swap(&mut self, world: &mut World) {
                $(std::mem::swap(&mut self.$field, &mut *world.resource_mut::<$ty>());)*
            }

            #[cfg(test)]
            fn init(world: &mut World) {
                $(world.insert_resource(<$ty>::default());)*
            }
        }
    };
}

per_scene! {
    selection: crate::scene::Selection,
    pending: PendingSpawns,
    downloads: PendingDownloads,
    names: crate::scene::NameCounter,
    drag: crate::scene::DragState,
    handles: crate::scene::HandleUi,
    undo: UndoStack,
    menu: crate::context_menu::ContextMenu,
    links: crate::plant::Links,
    plant: crate::plant::Plant,
    rerouting: crate::plant::reroute::Rerouting,
    rtls: crate::rtls::RtlsState,
    rtls_selection: crate::rtls::RtlsSelection,
    coverage: crate::rtls::planning::Coverage,
    applied_antenna: crate::rtls::planning::AppliedAntenna,
    plan_jobs: crate::rtls::ui::PlanJobs,
    deploy: crate::rtls::ui::DeployState,
    smart_plan: crate::rtls::smart::SmartPlan,
    rtls_ui: crate::rtls::ui::RtlsUiState,
    bindings: crate::rtls::actuate::EmsBindings,
    tag_history: crate::rtls::tag_card::TagHistory,
    rf_history: crate::rtls::rf::RfHistory,
    fdtd: crate::rtls::fdtd::Fdtd,
    twin: crate::twin::TwinScene,
    twin_selection: crate::twin::TwinSelection,
}

#[derive(Clone, Copy)]
struct CameraPose {
    focus: Vec3,
    yaw: f32,
    pitch: f32,
    radius: f32,
}

impl Default for CameraPose {
    /// Same view as a fresh start of the app.
    fn default() -> Self {
        Self { focus: Vec3::new(0.0, 0.5, 0.0), yaw: 0.65, pitch: 0.45, radius: 5.0 }
    }
}

pub struct Tab {
    pub id: u64,
    pub title: String,
    pub path: Option<PathBuf>,
    /// Has changes a close would lose. Kept up to date while the tab shows.
    pub unsaved: bool,
    /// Layout to read the first time the tab shows.
    lazy: Option<PathBuf>,
    frame_after_load: Option<u32>,
    camera: CameraPose,
    stash: Box<Stash>,
    /// Calibrations live outside the world (every ray cast reads them), so they move by hand.
    cals: crate::rtls::radio::ParkedCals,
    /// Its workspace and the cards it had open. Holds a blank while the tab shows.
    view: ProjectView,
}

impl Tab {
    fn new(id: u64) -> Self {
        Self { id, title: String::new(), path: None, unsaved: false, lazy: None, frame_after_load: None, camera: CameraPose::default(), stash: Box::default(), cals: Default::default(), view: ProjectView::default() }
    }

    /// Opened, but its layout has not been read yet.
    pub fn not_loaded(&self) -> bool {
        self.lazy.is_some()
    }
}

/// A tab's name for a layout file.
pub fn title_of(path: &std::path::Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

/// A scene to open in a tab.
pub struct NewTab {
    pub title: String,
    /// The file saving goes to; `None` asks for one.
    pub path: Option<PathBuf>,
    /// The layout to read.
    pub load: Option<PathBuf>,
}

pub enum Request {
    /// Opens these; shows the first (or the tab that already has its file).
    Open(Vec<NewTab>),
    /// Adds tabs without leaving the one showing.
    OpenInBackground(Vec<NewTab>),
    Activate(u64),
    /// To the next (+1) or previous (-1) tab, wrapping around.
    Step(i32),
    Close(u64),
    /// Closes the showing tab unless its save failed.
    CloseIfSaved(u64),
    /// Closes all others with nothing to lose.
    CloseOthers(u64),
}

#[derive(Resource)]
pub struct Tabs {
    list: Vec<Tab>,
    active: usize,
    next_id: u64,
    requests: VecDeque<Request>,
    /// Roots of closed tabs, still to be freed.
    trash: Vec<Entity>,
    /// Drop assets nothing uses any more once `trash` is empty.
    evict: bool,
    /// The scene changed under the systems this frame (not an edit).
    switched: bool,
    /// The tab strip should bring the showing tab into view.
    pub reveal_active: bool,
}

impl Default for Tabs {
    fn default() -> Self {
        Self { list: vec![Tab::new(1)], active: 0, next_id: 2, requests: VecDeque::new(), trash: Vec::new(), evict: false, switched: false, reveal_active: false }
    }
}

impl Tabs {
    pub fn list(&self) -> &[Tab] {
        &self.list
    }

    pub fn active_id(&self) -> u64 {
        self.list[self.active].id
    }

    pub fn get(&self, id: u64) -> Option<&Tab> {
        self.list.iter().find(|t| t.id == id)
    }

    pub fn request(&mut self, r: Request) {
        self.requests.push_back(r);
    }

    /// What the top bar shows for the current scene, copied each frame.
    pub fn mirror(&mut self, title: &str, path: Option<&PathBuf>, unsaved: bool) {
        let t = &mut self.list[self.active];
        if t.title != title {
            t.title = title.to_string();
        }
        if t.path.as_ref() != path {
            t.path = path.cloned();
        }
        t.unsaved = unsaved;
    }

    fn position(&self, id: u64) -> Option<usize> {
        self.list.iter().position(|t| t.id == id)
    }

    fn activate(&mut self, world: &mut World, id: u64) {
        let Some(i) = self.position(id).filter(|i| *i != self.active) else { return };
        self.leave_active(world);
        self.active = i;
        self.enter_active(world);
    }

    /// Parks the showing tab; the world is left with a blank scene.
    fn leave_active(&mut self, world: &mut World) {
        let tab = &mut self.list[self.active];
        // Before the swap below: the card it notes belongs to this scene.
        world.resource_scope(|world, mut slot: Mut<LeftPanelSlot>| tab.view = slot.take_view(world.resource::<RtlsUiState>()));
        let (title, path, frame) = world.resource_mut::<UiState>().into_inner().scene_fields();
        tab.title.clone_from(title);
        tab.path.clone_from(path);
        tab.frame_after_load = frame.take();
        if let Ok(o) = world.query::<&OrbitCamera>().single(world) {
            tab.camera = CameraPose { focus: o.focus, yaw: o.yaw, pitch: o.pitch, radius: o.radius };
        }
        // The heat map entity belongs to the scene; the system that hides it only
        // knows the current map.
        if let Some(e) = world.resource::<crate::rtls::planning::Coverage>().heatmap()
            && let Some(mut v) = world.get_mut::<Visibility>(e)
        {
            *v = Visibility::Hidden;
        }
        park_entities(world, tab.id);
        tab.stash.swap(world);
        crate::rtls::radio::MaterialCal::swap_parked(&mut tab.cals);
        cancel_tools(world);
    }

    /// Unparks `self.active`.
    fn enter_active(&mut self, world: &mut World) {
        let tab = &mut self.list[self.active];
        tab.stash.swap(world);
        crate::rtls::radio::MaterialCal::swap_parked(&mut tab.cals);
        unpark_entities(world, tab.id);
        let view = std::mem::take(&mut tab.view);
        world.resource_scope(|world, mut slot: Mut<LeftPanelSlot>| slot.show_view(&mut world.resource_mut::<RtlsUiState>(), view));
        // The last message was about the scene just left.
        world.resource_mut::<Status>().0.clear();
        let (title, path, frame) = world.resource_mut::<UiState>().into_inner().scene_fields();
        title.clone_from(&tab.title);
        path.clone_from(&tab.path);
        *frame = tab.frame_after_load.take();
        if let Ok(mut o) = world.query::<&mut OrbitCamera>().single_mut(world) {
            o.focus = tab.camera.focus;
            o.yaw = tab.camera.yaw;
            o.pitch = tab.camera.pitch;
            o.radius = tab.camera.radius;
        }
        if let Some(load) = tab.lazy.take() {
            begin_load(world, load);
        }
        self.reveal_active = true;
        self.switched = true;
    }

    fn open(&mut self, world: &mut World, new: Vec<NewTab>, background: bool) {
        let mut show = None;
        let asked = std::mem::take(&mut world.resource_mut::<LeftPanelSlot>().bench.opening);
        // The blank scene of a fresh start (or an emptied one) is used up first.
        let mut reuse = !background && self.active_is_blank(world);
        for n in new {
            if let Some(t) = n.path.as_ref().and_then(|p| self.list.iter().find(|t| t.path.as_ref() == Some(p))) {
                show.get_or_insert(t.id);
                continue;
            }
            if reuse {
                reuse = false;
                let (title, path, _) = world.resource_mut::<UiState>().into_inner().scene_fields();
                title.clone_from(&n.title);
                path.clone_from(&n.path);
                // The user opened it, so it starts over in its own workspace. A scripted
                // run has already gone to the one it wants.
                if asked {
                    let view = ProjectView::fresh(n.load.is_some());
                    world.resource_scope(|world, mut slot: Mut<LeftPanelSlot>| slot.show_view(&mut world.resource_mut::<RtlsUiState>(), view));
                }
                if let Some(load) = n.load {
                    begin_load(world, load);
                }
                show.get_or_insert(self.active_id());
                continue;
            }
            let mut tab = Tab::new(self.next_id);
            self.next_id += 1;
            tab.title = n.title;
            tab.path = n.path;
            tab.view = ProjectView::fresh(asked && n.load.is_some());
            tab.lazy = n.load;
            tab.stash.rtls = crate::rtls::RtlsState::new(world.resource::<crate::rtls::RtlsConfig>().settings.seed);
            show.get_or_insert(tab.id);
            self.list.push(tab);
        }
        if let Some(id) = show.filter(|_| !background) {
            self.activate(world, id);
        }
    }

    fn active_is_blank(&self, world: &mut World) -> bool {
        let t = &self.list[self.active];
        t.path.is_none()
            && t.lazy.is_none()
            && !world.resource::<UndoStack>().dirty()
            && world.resource::<PendingSpawns>().0.is_empty()
            && world.resource::<PendingDownloads>().0.is_empty()
            && world.query_filtered::<(), With<PlacedObject>>().iter(world).next().is_none()
            && world.query_filtered::<(), With<Device>>().iter(world).next().is_none()
            && world.query_filtered::<(), With<Zone>>().iter(world).next().is_none()
    }

    fn close(&mut self, world: &mut World, id: u64) {
        let Some(i) = self.position(id) else { return };
        let was_active = i == self.active;
        if was_active {
            self.leave_active(world);
        }
        let tab = self.list.remove(i);
        collect_trash(world, tab.id, &mut self.trash);
        self.trash.extend(tab.stash.coverage.heatmap());
        self.evict = true;
        if was_active {
            if self.list.is_empty() {
                self.list.push(Tab::new(self.next_id));
                self.next_id += 1;
                self.active = 0;
                self.enter_active(world);
                world.resource_mut::<UiState>().ask_for_scene();
            } else {
                self.active = i.min(self.list.len() - 1);
                self.enter_active(world);
            }
        } else if i < self.active {
            self.active -= 1;
        }
    }

    fn handle(&mut self, world: &mut World, request: Request) {
        match request {
            Request::Open(new) => self.open(world, new, false),
            Request::OpenInBackground(new) => self.open(world, new, true),
            Request::Activate(id) => self.activate(world, id),
            Request::Step(by) => {
                let n = self.list.len() as i32;
                let id = self.list[(self.active as i32 + by).rem_euclid(n) as usize].id;
                self.activate(world, id);
            }
            Request::Close(id) => self.close(world, id),
            Request::CloseIfSaved(id) => {
                if id == self.active_id() && world.resource::<UndoStack>().dirty() {
                    let mut status = world.resource_mut::<Status>();
                    status.0 = format!("{} · the tab stays open", status.0);
                } else {
                    self.close(world, id);
                }
            }
            Request::CloseOthers(id) => {
                self.activate(world, id);
                let others: Vec<u64> = self.list.iter().filter(|t| t.id != id && !t.unsaved).map(|t| t.id).collect();
                for t in others {
                    self.close(world, t);
                }
                let kept = self.list.len() - 1;
                if kept > 0 {
                    world.resource_mut::<Status>().0 = format!("Kept {kept} tab(s) with unsaved changes");
                }
            }
        }
    }
}

fn begin_load(world: &mut World, layout: PathBuf) {
    world.resource_mut::<UndoStack>().rebase();
    world.resource_mut::<Actions>().0.push(Action::LoadLayout(layout));
    world.resource_mut::<UiState>().frame_on_load();
}

/// Tools act on the scene they started in.
fn cancel_tools(world: &mut World) {
    world.resource_mut::<crate::scene::PlaceMode>().asset = None;
    let mut device = world.resource_mut::<crate::rtls::DevicePlace>();
    device.profile = None;
    device.waypoints_for = None;
    device.preview = None;
    world.resource_mut::<crate::wall_tool::WallTool>().active = false;
    *world.resource_mut::<crate::undo::UndoRequest>() = default();
}

fn park_entities(world: &mut World, owner: u64) {
    let roots: Vec<Entity> = world.query_filtered::<Entity, With<PlacedObject>>().iter(world).collect();
    let set: HashSet<Entity> = roots.iter().copied().collect();
    let parts: Vec<Entity> = world.query_filtered::<(Entity, &PartOf), With<PartBvh>>().iter(world).filter(|(_, of)| set.contains(&of.0)).map(|(e, _)| e).collect();
    for e in parts {
        world.entity_mut(e).remove::<PartBvh>();
    }
    // A tank's liquid column is rebuilt by `plant::view::sync_liquid` on return.
    let columns: Vec<Entity> = world.query_filtered::<(Entity, &ChildOf), With<crate::plant::view::Liquid>>().iter(world).filter(|(_, c)| set.contains(&c.parent())).map(|(e, _)| e).collect();
    for e in columns {
        world.entity_mut(e).despawn();
    }
    for e in roots {
        let mut entity = world.entity_mut(e);
        let Some(object) = entity.take::<PlacedObject>() else { continue };
        let visibility = entity.get::<Visibility>().copied().unwrap_or_default();
        entity.insert((ParkedObject { owner, object, visibility }, Visibility::Hidden));
    }
    let devices: Vec<Entity> = world.query_filtered::<Entity, With<Device>>().iter(world).collect();
    for e in devices {
        let mut entity = world.entity_mut(e);
        let Some(device) = entity.take::<Device>() else { continue };
        let visibility = entity.get::<Visibility>().copied();
        entity.insert((ParkedDevice { owner, device, visibility }, Visibility::Hidden));
    }
    let zones: Vec<Entity> = world.query_filtered::<Entity, With<Zone>>().iter(world).collect();
    for e in zones {
        let mut entity = world.entity_mut(e);
        let Some(zone) = entity.take::<Zone>() else { continue };
        entity.insert(ParkedZone { owner, zone });
    }
}

fn unpark_entities(world: &mut World, owner: u64) {
    let roots: Vec<Entity> = world.query::<(Entity, &ParkedObject)>().iter(world).filter(|(_, p)| p.owner == owner).map(|(e, _)| e).collect();
    let mut protos: HashMap<Entity, Arc<Prototype>> = HashMap::with_capacity(roots.len());
    for e in roots {
        let mut entity = world.entity_mut(e);
        let Some(p) = entity.take::<ParkedObject>() else { continue };
        protos.insert(e, p.object.proto.clone());
        entity.insert((p.object, p.visibility));
    }
    let parts: Vec<(Entity, Arc<crate::bvh::Bvh>)> = world
        .query_filtered::<(Entity, &PartOf, &PartRest), Without<PartBvh>>()
        .iter(world)
        .filter_map(|(e, of, rest)| Some((e, protos.get(&of.0)?.parts.get(rest.0)?.bvh.clone())))
        .collect();
    for (e, bvh) in parts {
        world.entity_mut(e).insert(PartBvh(bvh));
    }
    let devices: Vec<Entity> = world.query::<(Entity, &ParkedDevice)>().iter(world).filter(|(_, p)| p.owner == owner).map(|(e, _)| e).collect();
    for e in devices {
        let mut entity = world.entity_mut(e);
        let Some(p) = entity.take::<ParkedDevice>() else { continue };
        entity.insert((p.device, p.visibility.unwrap_or_default()));
    }
    let zones: Vec<Entity> = world.query::<(Entity, &ParkedZone)>().iter(world).filter(|(_, p)| p.owner == owner).map(|(e, _)| e).collect();
    for e in zones {
        let mut entity = world.entity_mut(e);
        if let Some(p) = entity.take::<ParkedZone>() {
            entity.insert(p.zone);
        }
    }
}

fn collect_trash(world: &mut World, owner: u64, trash: &mut Vec<Entity>) {
    trash.extend(world.query::<(Entity, &ParkedObject)>().iter(world).filter(|(_, p)| p.owner == owner).map(|(e, _)| e));
    trash.extend(world.query::<(Entity, &ParkedDevice)>().iter(world).filter(|(_, p)| p.owner == owner).map(|(e, _)| e));
    trash.extend(world.query::<(Entity, &ParkedZone)>().iter(world).filter(|(_, p)| p.owner == owner).map(|(e, _)| e));
}

/// Runs first in the frame, so every system sees one consistent scene.
pub fn apply_requests(world: &mut World) {
    world.resource_scope(|world, mut tabs: Mut<Tabs>| {
        free_closed_tabs(world, &mut tabs);
        if tabs.requests.is_empty() {
            return;
        }
        // The assistant works on the scene it was asked about.
        if world.resource::<crate::agent::Agent>().running {
            tabs.requests.clear();
            world.resource_mut::<Status>().0 = "The assistant is working on this scene: stop it to change tabs".into();
            return;
        }
        // Actions queued by the UI belong to the scene they were asked in; a drag to
        // the scene it started in.
        while !tabs.requests.is_empty() && world.resource::<Actions>().0.is_empty() && world.resource::<crate::scene::DragState>().holding().is_none() {
            let Some(request) = tabs.requests.pop_front() else { break };
            let started = std::time::Instant::now();
            tabs.handle(world, request);
            debug!("tab change took {:.2} ms", started.elapsed().as_secs_f32() * 1000.0);
        }
    });
}

fn free_closed_tabs(world: &mut World, tabs: &mut Tabs) {
    let keep = tabs.trash.len().saturating_sub(DESPAWN_PER_FRAME);
    for e in tabs.trash.split_off(keep) {
        if let Ok(entity) = world.get_entity_mut(e) {
            entity.despawn();
        }
    }
    if tabs.evict && tabs.trash.is_empty() {
        tabs.evict = false;
        // Assets only the cache still holds go, freeing their meshes and textures.
        let placing = world.resource::<crate::scene::PlaceMode>().asset.clone();
        world.resource_mut::<PrototypeCache>().states.retain(|path, s| !matches!(s, ProtoState::Ready(p) if Arc::strong_count(p) == 1) || placing.as_ref() == Some(path));
    }
}

/// Devices and zones are outside the undo snapshots, but saving covers them.
#[allow(clippy::too_many_arguments)]
pub fn track_rtls_edits(
    mut tabs: ResMut<Tabs>,
    mut undo: ResMut<UndoStack>,
    devices: Query<(Ref<Device>, Ref<Transform>)>,
    zones: Query<Ref<Zone>>,
    mut gone_devices: RemovedComponents<Device>,
    mut gone_zones: RemovedComponents<Zone>,
    parked_devices: Query<(), With<ParkedDevice>>,
    parked_zones: Query<(), With<ParkedZone>>,
) {
    let removed = gone_devices.read().filter(|e| !parked_devices.contains(*e)).count() + gone_zones.read().filter(|e| !parked_zones.contains(*e)).count();
    // Tags move on their own; that is the simulation, not an edit.
    let edited = removed > 0 || devices.iter().any(|(d, t)| d.is_changed() || (!d.kind.is_mobile() && t.is_changed())) || zones.iter().any(|z| z.is_changed());
    // Coming back from parked also reads as changed.
    if std::mem::take(&mut tabs.switched) {
        return;
    }
    if edited {
        undo.touch();
    }
}

pub struct TabsPlugin;

impl Plugin for TabsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Tabs>().add_systems(First, apply_requests).add_systems(Update, track_rtls_edits);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plant::connect::{Link, LinkKind, PortRef};
    use crate::plant::{Links, persist};
    use crate::scene::Selection;

    fn world() -> World {
        let mut w = World::new();
        Stash::init(&mut w);
        w.init_resource::<Tabs>();
        w.init_resource::<UiState>();
        w.init_resource::<Actions>();
        w.init_resource::<Status>();
        w.init_resource::<crate::rtls::RtlsConfig>();
        w.init_resource::<crate::scene::PlaceMode>();
        w.init_resource::<crate::rtls::DevicePlace>();
        w.init_resource::<crate::wall_tool::WallTool>();
        w.init_resource::<crate::undo::UndoRequest>();
        w.init_resource::<PrototypeCache>();
        w.init_resource::<LeftPanelSlot>();
        w.spawn(OrbitCamera { focus: Vec3::ZERO, yaw: 0.0, pitch: 0.0, radius: 9.0 });
        w
    }

    /// An object with one mesh part, as `scene::spawn_object` builds it.
    fn object(w: &mut World, name: &str) -> Entity {
        let bvh = Arc::new(crate::bvh::Bvh::new(&[[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]], &[0, 1, 2]));
        let proto = Arc::new(Prototype {
            parts: vec![crate::prototype::ProtoPart {
                body: None,
                name: "part".into(),
                prim_path: String::new(),
                mesh: Handle::default(),
                material: Handle::default(),
                transform: Transform::IDENTITY,
                bvh: bvh.clone(),
                aabb: default(),
                triangles: 1,
                lods: Vec::new(),
                see_through: false,
            }],
            joints: Vec::new(),
            links: Vec::new(),
            unit: 1.0,
            basis: Transform::IDENTITY,
            aabb_min: Vec3::ZERO,
            aabb_max: Vec3::ONE,
            triangles: 1,
            load_ms: 0.0,
            warnings: Vec::new(),
            detected: default(),
        });
        let root = w.spawn((Transform::IDENTITY, Visibility::Visible, PlacedObject { asset: "a.usd".into(), name: name.into(), proto, joints: Vec::new(), locked: false })).id();
        w.spawn((PartOf(root), PartRest(0), PartBvh(bvh), ChildOf(root)));
        root
    }

    #[test]
    fn a_tab_comes_back_in_the_workspace_it_was_left_in() {
        use crate::ui::Workbench;
        let mut w = world();
        let a = with_tabs(&mut w, |_, t| t.active_id());
        // With something in it, so the next project gets a tab of its own.
        object(&mut w, "a");
        w.resource_mut::<LeftPanelSlot>().bench.current = Workbench::Factory;

        let b = open(&mut w, "b");
        assert_ne!(a, b);
        assert_eq!(w.resource::<LeftPanelSlot>().bench.current, Workbench::Layout, "a new project starts in its own place");
        w.resource_mut::<LeftPanelSlot>().bench.current = Workbench::Plant;

        with_tabs(&mut w, |w, t| t.activate(w, a));
        assert_eq!(w.resource::<LeftPanelSlot>().bench.current, Workbench::Factory);
        with_tabs(&mut w, |w, t| t.activate(w, b));
        assert_eq!(w.resource::<LeftPanelSlot>().bench.current, Workbench::Plant);
    }

    #[test]
    fn only_a_project_the_user_opened_will_pick_its_workspace() {
        let mut w = world();
        object(&mut w, "a");
        open(&mut w, "scripted");
        assert!(!w.resource::<LeftPanelSlot>().bench.settle_on_load);

        object(&mut w, "b");
        w.resource_mut::<LeftPanelSlot>().bench.opening = true;
        open(&mut w, "chosen");
        let slot = w.resource::<LeftPanelSlot>();
        assert!(slot.bench.settle_on_load && !slot.bench.opening);
    }

    #[test]
    fn a_scripted_load_into_the_blank_scene_keeps_the_workspace_it_went_to() {
        use crate::ui::Workbench;
        let mut w = world();
        w.resource_mut::<LeftPanelSlot>().bench.current = Workbench::Simulation;
        open(&mut w, "scripted");
        assert_eq!(w.resource::<LeftPanelSlot>().bench.current, Workbench::Simulation);
    }

    #[test]
    fn a_chosen_project_in_the_blank_scene_starts_over() {
        use crate::ui::Workbench;
        let mut w = world();
        let mut slot = w.resource_mut::<LeftPanelSlot>();
        slot.bench.current = Workbench::Factory;
        slot.bench.opening = true;
        open(&mut w, "chosen");
        assert_eq!(w.resource::<LeftPanelSlot>().bench.current, Workbench::Layout);
    }

    fn with_tabs<R>(w: &mut World, f: impl FnOnce(&mut World, &mut Tabs) -> R) -> R {
        w.resource_scope(|w, mut t: Mut<Tabs>| f(w, &mut t))
    }

    fn open(w: &mut World, name: &str) -> u64 {
        let path = PathBuf::from(format!("{name}.json"));
        with_tabs(w, |w, t| {
            t.open(w, vec![NewTab { title: name.into(), path: Some(path.clone()), load: Some(path) }], false);
            t.active_id()
        })
    }

    fn count<C: Component>(w: &mut World) -> usize {
        w.query_filtered::<(), With<C>>().iter(w).count()
    }

    #[test]
    fn a_parked_scene_is_out_of_every_query_and_comes_back_whole() {
        let mut w = world();
        let first = with_tabs(&mut w, |_, t| t.active_id());
        let a = object(&mut w, "a");
        w.resource_mut::<Selection>().set(Some(a));
        w.query::<&mut OrbitCamera>().single_mut(&mut w).unwrap().radius = 42.0;

        open(&mut w, "b");
        assert_eq!(count::<PlacedObject>(&mut w), 0);
        // Ray casts read parts directly: a hidden wall must not block a visible signal.
        assert_eq!(count::<PartBvh>(&mut w), 0);
        assert_eq!(*w.get::<Visibility>(a).unwrap(), Visibility::Hidden);
        assert_eq!(w.resource::<Selection>().selected, None);
        assert_eq!(w.query::<&OrbitCamera>().single(&w).unwrap().radius, 5.0);

        with_tabs(&mut w, |w, t| t.activate(w, first));
        assert_eq!(count::<PlacedObject>(&mut w), 1);
        assert_eq!(count::<PartBvh>(&mut w), 1);
        assert_eq!(*w.get::<Visibility>(a).unwrap(), Visibility::Visible);
        assert_eq!(w.resource::<Selection>().selected, Some(a));
        assert_eq!(w.query::<&OrbitCamera>().single(&w).unwrap().radius, 42.0);
    }

    #[test]
    fn calibrations_stay_with_their_tab() {
        use crate::rtls::radio::{CAL_TEST_LOCK, CalBand, CalSource, Material, MaterialCal, SWAPS_HERE};
        let _g = CAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        SWAPS_HERE.set(true);
        let mut w = world();
        let first = with_tabs(&mut w, |_, t| t.active_id());
        object(&mut w, "a");
        let cal = MaterialCal { loss_scale: 2.0, source: CalSource { band: CalBand::Ghz8, ..Default::default() }, ..Default::default() };
        MaterialCal::set(Material::Wood, Some(cal));
        let second = open(&mut w, "b");
        let in_b = MaterialCal::scales_in(Material::Wood, CalBand::Ghz8);
        MaterialCal::set(Material::Wood, Some(MaterialCal { loss_scale: 3.0, source: CalSource { band: CalBand::Ghz8, ..Default::default() }, ..Default::default() }));
        with_tabs(&mut w, |w, t| t.activate(w, first));
        let back_in_a = MaterialCal::scales_in(Material::Wood, CalBand::Ghz8);
        with_tabs(&mut w, |w, t| t.activate(w, second));
        let back_in_b = MaterialCal::scales_in(Material::Wood, CalBand::Ghz8);
        MaterialCal::clear_bands();
        assert_eq!(in_b, (1.0, 1.0), "a new tab starts on the built-in values");
        assert_eq!(back_in_a, (2.0, 1.0));
        assert_eq!(back_in_b, (3.0, 1.0));
    }

    #[test]
    fn layouts_are_read_only_when_their_tab_first_shows() {
        let mut w = world();
        object(&mut w, "a");
        let new = ["b", "c"].map(|n| NewTab { title: n.into(), path: Some(format!("{n}.json").into()), load: Some(format!("{n}.json").into()) }).into();
        with_tabs(&mut w, |w, t| t.open(w, new, true));
        assert!(w.resource::<Actions>().0.is_empty());

        let c = with_tabs(&mut w, |_, t| t.list()[2].id);
        with_tabs(&mut w, |w, t| t.activate(w, c));
        assert!(matches!(w.resource::<Actions>().0.as_slice(), [Action::LoadLayout(p)] if p.ends_with("c.json")));
        assert!(with_tabs(&mut w, |_, t| t.list()[1].not_loaded()));
    }

    #[test]
    fn a_file_already_open_is_shown_not_opened_twice() {
        let mut w = world();
        object(&mut w, "a");
        let b = open(&mut w, "b");
        open(&mut w, "c");
        assert_eq!(open(&mut w, "b"), b);
        assert_eq!(with_tabs(&mut w, |_, t| t.list().len()), 3);
    }

    #[test]
    fn a_blank_scene_is_used_up_by_the_first_open() {
        let mut w = world();
        let first = with_tabs(&mut w, |_, t| t.active_id());
        assert_eq!(open(&mut w, "b"), first);
        assert_eq!(with_tabs(&mut w, |_, t| t.list().len()), 1);
    }

    #[test]
    fn parking_a_scene_does_not_delete_links_of_the_same_name_elsewhere() {
        let mut w = world();
        let mut track = Schedule::default();
        track.add_systems(persist::track_names);
        object(&mut w, "tank_1");
        track.run(&mut w);

        // The next scene has an object of the same name, linked.
        let first = with_tabs(&mut w, |_, t| t.active_id());
        let second = open(&mut w, "b");
        w.resource_mut::<Links>().add(Link { a: PortRef::new("tank_1", "outlet"), b: PortRef::new("pipe_1", "a"), kind: LinkKind::Pipe });

        // Parks the first scene's tank while the second scene's links are in place.
        track.run(&mut w);
        assert_eq!(w.resource::<Links>().0.len(), 1);

        // And each scene keeps its own links.
        with_tabs(&mut w, |w, t| t.activate(w, first));
        assert_eq!(w.resource::<Links>().0.len(), 0);
        with_tabs(&mut w, |w, t| t.activate(w, second));
        track.run(&mut w);
        assert_eq!(w.resource::<Links>().0.len(), 1);
    }

    #[test]
    fn closing_frees_the_scene_and_the_last_close_leaves_a_blank_one() {
        let mut w = world();
        let a = object(&mut w, "a");
        let first = with_tabs(&mut w, |_, t| t.active_id());
        let second = open(&mut w, "b");
        let b = object(&mut w, "b");

        // Closing a tab that is not showing leaves the showing one alone.
        with_tabs(&mut w, |w, t| t.close(w, first));
        with_tabs(&mut w, |w, t| free_closed_tabs(w, t));
        assert!(w.get_entity(a).is_err());
        assert!(w.get::<PlacedObject>(b).is_some());

        with_tabs(&mut w, |w, t| t.close(w, second));
        with_tabs(&mut w, |w, t| free_closed_tabs(w, t));
        assert!(w.get_entity(b).is_err());
        assert_eq!(count::<PlacedObject>(&mut w), 0);
        let (n, id) = with_tabs(&mut w, |_, t| (t.list().len(), t.active_id()));
        assert_eq!(n, 1);
        assert!(id != first && id != second);
    }

    #[test]
    fn unsaved_follows_edits_and_saves() {
        let mut u = UndoStack::default();
        assert!(!u.dirty());
        u.touch();
        assert!(u.dirty());
        u.mark_saved();
        assert!(!u.dirty());
    }
}
