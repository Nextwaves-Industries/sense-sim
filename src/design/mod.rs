//! The Design workspace: SolidCraft's parametric CAD, inside Sense Sim's window.
//!
//! SolidCraft runs in its own egui context on its own GPU device (`host`) and its picture is shown
//! here as an image over the whole window. While it is open, keyboard input goes to it alone, the
//! 3D camera is off, and scene tools are blocked.

pub mod autotest;
pub mod bridge;
pub mod host;
pub mod input;
pub mod shell;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy_egui::{EguiContexts, EguiInput, EguiPreUpdateSet, EguiPrimaryContextPass, EguiTextureHandle, PrimaryEguiContext};

use crate::scene::{OrbitCamera, UiBlock};
use crate::ui::LeftPanelSlot;
use host::DesignHost;

const STRIP_HEIGHT: f32 = 34.0;
/// The shell has its own way out of the workspace, so the strip shows only when it has something to
/// say: a face pick in progress, or a failure.
const SHELL_HAS_WAY_BACK: bool = true;

/// What fills the window.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Workspace {
    #[default]
    Scene,
    Design,
}

/// Input taken from the host context this frame, before its UI could act on it.
#[derive(Default)]
struct Stolen {
    events: Vec<egui::Event>,
    dropped: Vec<egui::DroppedFileHandle>,
    hovered: Vec<egui::HoveredFile>,
}

/// SolidCraft's state is not `Send` (its host services are boxed closures), so this stays on the
/// main thread. `Err` once the workspace has failed: it says why and stays off.
#[derive(Default)]
pub struct Slot {
    pub host: Option<DesignHost>,
    pub failed: Option<String>,
    /// While set, the workspace is open to pick faces for a simulation: shown in the strip.
    pub prompt: Option<String>,
    /// Set when the user says they're done picking.
    pub prompt_done: bool,
    /// A design to open once the workspace first starts: a loaded layout's.
    pub open_when_started: Option<std::path::PathBuf>,
    /// Unsaved work was brought back when it started: still to be said.
    restored_note: bool,
    /// Set when the licences were asked for.
    pub want_notices: bool,
    image: Option<Handle<Image>>,
    texture: Option<egui::TextureId>,
    /// The drawn image's size in pixels: the image is replaced when it changes.
    image_size: [u32; 2],
    pressed: u8,
    next_repaint: Option<Instant>,
    /// Frames drawn, and the slowest and total time they took, for the test run's report.
    pub drawn: u64,
    pub total_ms: f64,
    pub worst_ms: f64,
}

pub struct DesignPlugin;

impl Plugin for DesignPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Workspace>()
            .insert_non_send(Stolen::default())
            .insert_non_send(Slot::default())
            .add_systems(PreUpdate, steal_input.after(EguiPreUpdateSet::ProcessInput).before(EguiPreUpdateSet::BeginPass))
            .add_systems(Update, (quit_when_asked, save_scene_on_entry))
            .add_systems(Last, protect_work)
            .add_systems(EguiPrimaryContextPass, (sync_camera.after(crate::ui::ui_system), draw.after(crate::ui::ui_system).before(crate::ime::sync_ime)));
    }
}

/// Where the scene is saved each time the workspace opens.
pub fn scene_safety_path() -> std::path::PathBuf {
    autosave_path().with_file_name("scene-before-design.json")
}

/// The CAD kernel runs in this process. `catch_unwind` holds its panics, but nothing holds an abort or a
/// bad access, so the scene is written out on the way in. It can be opened like any saved layout.
fn save_scene_on_entry(workspace: Res<Workspace>, mut last: Local<Workspace>, objects: Query<(), With<crate::scene::PlacedObject>>, mut actions: ResMut<crate::scene::Actions>) {
    if *workspace == Workspace::Design && *last != Workspace::Design && !objects.is_empty() {
        let path = scene_safety_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        actions.0.push(crate::scene::Action::SaveLayout(path));
    }
    *last = *workspace;
}

/// Set by File > Quit, which runs where it can't reach the app's exit event.
static QUIT: AtomicBool = AtomicBool::new(false);

/// Ends the app the orderly way, so that unsaved design work is protected first.
pub fn request_quit() {
    QUIT.store(true, Ordering::SeqCst);
}

fn quit_when_asked(mut exit: MessageWriter<AppExit>) {
    if QUIT.swap(false, Ordering::SeqCst) {
        exit.write(AppExit::Success);
    }
}

/// Set for test runs: see [`use_test_autosave`].
static TEST_AUTOSAVE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Test runs keep an autosave file of their own, emptied now. A test's parts must not come back as the
/// user's unsaved work next time, and a test must not take the user's for its own, or delete it.
pub fn use_test_autosave() {
    let path = TEST_AUTOSAVE.get_or_init(|| std::env::temp_dir().join("sense-sim-test-autosave.sc"));
    let _ = std::fs::remove_file(path);
}

/// Where unsaved work is kept when Sense Sim ends: reopened with the workspace next time.
pub fn autosave_path() -> std::path::PathBuf {
    if let Some(path) = TEST_AUTOSAVE.get() {
        return path.clone();
    }
    std::env::var_os("LOCALAPPDATA").map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from).join("SenseSim").join("design").join("autosave.sc")
}

/// On the way out: a design with unsaved changes goes to the autosave file, and SolidCraft finishes
/// what it was doing.
fn protect_work(mut exits: MessageReader<AppExit>, mut slot: NonSendMut<Slot>) {
    if exits.read().next().is_none() {
        return;
    }
    let Some(h) = slot.host.as_mut() else { return };
    if h.app.session.is_dirty() {
        let path = autosave_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let params = serde_json::json!({ "path": path.to_string_lossy() });
        if let Err(e) = h.app.session.execute("file.save_as", &params) {
            eprintln!("could not autosave the design: {e}");
        }
    }
    h.shutdown();
}

/// The work left in the autosave file, if any. Taken once: a clean run removes it.
fn restored_session() -> Option<solidcraft_engine::Session> {
    let path = autosave_path();
    let bytes = std::fs::read(&path).ok()?;
    let doc = solidcraft_engine::io::read_design(&bytes).ok()?;
    let _ = std::fs::remove_file(&path);
    let mut session = solidcraft_engine::Session::opened_at(doc, None, None);
    session.mark_unsaved();
    Some(session)
}

#[cfg(test)]
mod autosave_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unsaved_work_comes_back_once_still_marked_unsaved() {
        use_test_autosave();
        assert!(autosave_path().starts_with(std::env::temp_dir()), "tests stay out of the user's real autosave");
        assert!(restored_session().is_none(), "nothing to bring back yet");

        let mut session = solidcraft_engine::Session::default();
        let script = json!({"commands": [
            {"command": "sketch.create", "params": {"plane": "XY", "name": "S"}},
            {"command": "sketch.rectangle.two_point", "params": {"p0": [0, 0], "p1": [10, 10]}},
            {"command": "sketch.finish", "params": {}},
            {"command": "solid.extrude", "params": {"distance": "5 mm", "body_name": "Saved"}},
        ]});
        session.run_script(&script).unwrap();
        let path = autosave_path();
        session.execute("file.save_as", &json!({ "path": path.to_string_lossy() })).unwrap();

        let back = restored_session().expect("the work comes back");
        assert_eq!(bridge::bodies(&back), ["Saved"]);
        assert!(back.is_dirty(), "it was never saved by the user, so closing must still protect it");
        assert!(!path.exists(), "taken once");
        assert!(restored_session().is_none(), "and not again");
    }
}

/// What the workspace starts with: unsaved work first, then a loaded layout's design.
fn start_host(slot: &mut Slot, size: [u32; 2]) {
    let restored = restored_session();
    let was_restored = restored.is_some();
    let session = restored.or_else(|| {
        let file = slot.open_when_started.take()?;
        let doc = solidcraft_engine::io::read_design(&std::fs::read(&file).ok()?).ok()?;
        Some(solidcraft_engine::Session::opened_at(doc, None, file.to_str()))
    });
    let opened = session.is_some();
    match DesignHost::new(session.unwrap_or_default(), size, true) {
        Ok(mut host) => {
            if opened {
                host.app.home.open = false;
            }
            if was_restored {
                slot.restored_note = true;
                println!("restored the autosaved design: {} bodies", bridge::bodies(&host.app.session).len());
            }
            slot.host = Some(host);
        }
        Err(e) => slot.failed = Some(format!("The design workspace could not start: {e:#}")),
    }
}

/// The running design workspace, started if it wasn't: tools can use it without it being on screen.
pub fn ensure_host(slot: &mut Slot) -> Result<&mut DesignHost, String> {
    if slot.host.is_none() && slot.failed.is_none() {
        start_host(slot, [1600, 900]);
    }
    match (&mut slot.host, &slot.failed) {
        (Some(h), _) => Ok(h),
        (None, Some(e)) => Err(e.clone()),
        (None, None) => Err("the design workspace didn't start".to_string()),
    }
}

fn steal_input(workspace: Res<Workspace>, mut stolen: NonSendMut<Stolen>, mut inputs: Query<&mut EguiInput, With<PrimaryEguiContext>>) {
    stolen.events.clear();
    stolen.dropped.clear();
    stolen.hovered.clear();
    if *workspace != Workspace::Design {
        return;
    }
    for mut input in &mut inputs {
        stolen.dropped = std::mem::take(&mut input.0.dropped_files);
        stolen.hovered = std::mem::take(&mut input.0.hovered_files);
        // Pointer events stay too, for the strip above the workspace.
        stolen.events = input.0.events.clone();
        input.0.events.retain(|e| !input::is_keyboard(e));
    }
}

/// No rendering the scene behind the workspace. After the UI, and every frame while designing: the
/// shell can set this flag too, every frame, for views of its own.
fn sync_camera(workspace: Res<Workspace>, mut cameras: Query<&mut Camera, With<OrbitCamera>>) {
    let designing = *workspace == Workspace::Design;
    if designing || workspace.is_changed() {
        for mut camera in &mut cameras {
            camera.is_active = !designing;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw(
    mut contexts: EguiContexts,
    mut workspace: ResMut<Workspace>,
    mut slot: NonSendMut<Slot>,
    mut stolen: NonSendMut<Stolen>,
    mut images: ResMut<Assets<Image>>,
    mut block: ResMut<UiBlock>,
    mut panel: ResMut<LeftPanelSlot>,
    mut status: ResMut<crate::scene::Status>,
    mut pending: ResMut<crate::scene::PendingSpawns>,
) -> Result {
    panel.bench.design_ready = true;
    if workspace.is_changed() {
        panel.design_open = *workspace == Workspace::Design;
    } else {
        workspace.set_if_neq(if panel.design_open { Workspace::Design } else { Workspace::Scene });
    }
    if *workspace != Workspace::Design {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?.clone();
    let full = panel.studio_rect.unwrap_or(ctx.content_rect());
    let strip = !SHELL_HAS_WAY_BACK || slot.prompt.is_some() || slot.failed.is_some();
    let rect = egui::Rect::from_min_max(egui::pos2(full.left(), full.top() + if strip { STRIP_HEIGHT } else { 0.0 }), full.right_bottom());
    let ppp = ctx.pixels_per_point();
    let size = [(rect.width() * ppp).round().max(1.0) as u32, (rect.height() * ppp).round().max(1.0) as u32];

    // The strip: the way back.
    let mut back = false;
    let send = std::mem::take(&mut panel.design_send);
    slot.want_notices |= std::mem::take(&mut panel.design_licences);
    let mut done = false;
    if strip {
        egui::Area::new(egui::Id::new("design_strip")).order(egui::Order::Middle).fixed_pos(full.left_top()).show(&ctx, |ui| {
            egui::Frame::new().fill(crate::theme::PANEL).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
                ui.set_min_size(egui::vec2(full.width() - 16.0, STRIP_HEIGHT - 8.0));
                ui.horizontal(|ui| {
                    back = ui.button(crate::theme::label(crate::theme::icon::ARROW_U_UP_LEFT, "Back to scene")).clicked();
                    if let Some(p) = &slot.prompt {
                        ui.add_space(12.0);
                        ui.colored_label(crate::theme::ACCENT, p);
                        done = crate::theme::primary_button(ui, "Done").clicked();
                    } else if let Some(e) = &slot.failed {
                        ui.colored_label(crate::theme::DANGER, e);
                    } else if let Some(h) = &slot.host {
                        ui.label(egui::RichText::new(h.app.session.save_as_name()).color(crate::theme::WEAK));
                    }
                });
            });
        });
    }
    if done {
        slot.prompt_done = true;
        *workspace = Workspace::Scene;
        panel.design_open = false;
        return Ok(());
    }
    if back {
        // Leaving without finishing a pick drops it.
        slot.prompt = None;
        *workspace = Workspace::Scene;
        panel.design_open = false;
        return Ok(());
    }

    // Start on first use: fonts, device and pipelines take about a second.
    if slot.host.is_none() && slot.failed.is_none() {
        start_host(&mut slot, size);
    }
    if std::mem::take(&mut slot.restored_note) {
        status.0 = "Brought back the unsaved design from your last session.".into();
    }
    block.keyboard = true;
    block.pointer = true;

    if send && let Some(host) = &slot.host {
        match bridge_send(host, &mut pending) {
            Ok(bodies) => status.0 = format!("Placed {} design bodies in the scene", bodies.len()),
            Err(e) => status.0 = format!("Could not send the design to the scene: {e:#}"),
        }
    }

    let Slot { host, failed, pressed, next_repaint, .. } = &mut *slot;
    let Some(h) = host.as_mut() else { return Ok(()) };
    h.resize(size);
    if shell::exchange(&mut panel, &ctx, h, rect) {
        *next_repaint = None;
    }
    let zoom = h.ctx.zoom_factor();
    let events = input::translate(&stolen.events, rect, zoom, pressed);
    if std::env::var_os("SENSESIM_DESIGN_TRACE").is_some() && !stolen.events.is_empty() {
        println!("rect {rect:?} zoom {zoom} ppp {ppp} host {:?} -> private {events:?}", stolen.events);
    }
    let due = next_repaint.is_none_or(|t| Instant::now() >= t);
    let new_frame = !events.is_empty() || due || !stolen.dropped.is_empty() || !stolen.hovered.is_empty();
    if new_frame {
        let raw = egui::RawInput {
            events,
            focused: true,
            dropped_files: std::mem::take(&mut stolen.dropped),
            hovered_files: std::mem::take(&mut stolen.hovered),
            ..Default::default()
        };
        // The kernel turns its own panics into errors; this is for everything else.
        let started = Instant::now();
        match catch_unwind(AssertUnwindSafe(|| h.frame(raw, ppp))) {
            Ok(out) => {
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                forward(&ctx, out.platform, rect, zoom);
                *next_repaint = (out.repaint_after < Duration::from_secs(3600)).then(|| Instant::now() + out.repaint_after);
                slot.drawn += 1;
                slot.total_ms += ms;
                slot.worst_ms = slot.worst_ms.max(ms);
            }
            Err(_) => {
                // Its state can't be trusted now; the scene and Sense Sim go on.
                *failed = Some("The design workspace stopped after an internal error. Your scene is unaffected; reopen Sense Sim to use it again.".into());
                *host = None;
                return Ok(());
            }
        }
    }

    let Some(h) = slot.host.as_mut() else { return Ok(()) };
    if let Some((pixels, [w, h_px])) = h.take_pixels() {
        let replace = slot.image.is_none() || slot.image_size != [w, h_px];
        if replace {
            let mut image = Image::new(
                Extent3d { width: w, height: h_px, depth_or_array_layers: 1 },
                TextureDimension::D2,
                pixels,
                // Bytes in gamma space: this format makes the shader read them as light, as it expects.
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
            );
            image.sampler = bevy::image::ImageSampler::nearest();
            let handle = images.add(image);
            if let Some(old) = slot.image.take() {
                contexts.remove_image(&old);
            }
            slot.texture = Some(contexts.add_image(EguiTextureHandle::Strong(handle.clone())));
            slot.image = Some(handle);
            slot.image_size = [w, h_px];
        } else if let Some(mut image) = slot.image.as_ref().and_then(|handle| images.get_mut(handle)) {
            image.data = Some(pixels);
        }
    }

    if let Some(texture) = slot.texture {
        egui::Area::new(egui::Id::new("design_workspace")).order(egui::Order::Middle).fixed_pos(rect.min).interactable(false).show(&ctx, |ui| {
            ui.add(egui::Image::new(egui::load::SizedTexture::new(texture, rect.size())));
        });
    }
    Ok(())
}

/// What SolidCraft asked of the window: clipboard, links, cursor, input method.
fn forward(ctx: &egui::Context, platform: egui::PlatformOutput, rect: egui::Rect, zoom: f32) {
    ctx.set_cursor_icon(platform.cursor_icon);
    ctx.output_mut(|o| {
        o.commands.extend(platform.commands);
        o.ime = platform.ime.map(|mut ime| {
            ime.rect = egui::Rect::from_min_max(input::to_host(ime.rect.min, rect, zoom), input::to_host(ime.rect.max, rect, zoom));
            ime.cursor_rect = egui::Rect::from_min_max(input::to_host(ime.cursor_rect.min, rect, zoom), input::to_host(ime.cursor_rect.max, rect, zoom));
            ime
        });
    });
}

/// Places each body of the design in the scene, side by side, as generated USD assets. Returns their
/// names.
pub(crate) fn bridge_send(host: &DesignHost, pending: &mut crate::scene::PendingSpawns) -> anyhow::Result<Vec<String>> {
    let session = &host.app.session;
    let bodies = bridge::bodies(session);
    if bodies.is_empty() {
        anyhow::bail!("the design has no bodies yet");
    }
    for (i, body) in bodies.iter().enumerate() {
        let asset = bridge::bake(session, body)?;
        pending.0.push(crate::scene::PendingSpawn { asset, transform: Transform::from_xyz(i as f32 * 0.5, 0.0, 0.0), name: Some(body.clone()), joints: None, select: i == 0, locked: false });
    }
    Ok(bodies)
}
