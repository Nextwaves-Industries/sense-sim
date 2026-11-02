//! Sense Sim: browse NVIDIA SimReady USD assets, place them in a scene,
//! select/move/rotate them, play with their moving parts, and save or export
//! the layout.
//!
//! Usage: SenseSim [DATASET_ROOT] [--layout layout.json ... | --sample N]
//! Each further `--layout` opens in its own tab.

// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
#[cfg(feature = "antenna-studio")]
mod antenna;
mod bvh;
mod context_menu;
#[cfg(debug_assertions)]
mod drive;
mod gpu;
mod ime;
mod library;
mod lod;
mod mcp;
mod outcomes;
mod plant;
mod prototype;
mod remote;
mod rtls;
mod samples;
mod scene;
mod tabs;
mod theme;
mod ui;
mod undo;
mod usd_load;
mod wall_tool;

use bevy::camera::CameraOutputMode;
use bevy::camera::visibility::RenderLayers;
use bevy::diagnostic::FrameTimeDiagnosticsPlugin;
use bevy::ecs::system::NonSendMarker;
use bevy::picking::mesh_picking::MeshPickingPlugin;
use bevy::prelude::*;
use bevy::render::render_resource::BlendState;
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiGlobalSettings, EguiPlugin, EguiPrimaryContextPass, PrimaryEguiContext};

use library::Library;
use prototype::PrototypeCache;
use scene::*;

pub const APP_NAME: &str = "Sense Sim";

const UPLOAD_BYTES_PER_FRAME: usize = 48 * 1024 * 1024;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut root = None;
    let mut layouts: Vec<String> = Vec::new();
    let mut shot: Option<String> = None;
    let mut place: Vec<String> = Vec::new();
    let mut export: Option<String> = None;
    let mut pose = false;
    let mut preview = false;
    // `--start`: keep the start screen in a screenshot run.
    let mut keep_start = false;
    // `--open NAME`: open a card or dialog at the start (scripted screenshots).
    let mut open: Vec<String> = Vec::new();
    // `--no-select`: a screenshot run leaves the first object unselected.
    let mut no_select = false;
    let mut shot_delay = 8.0f32;
    // `--drive "wait:5;click:96,722"`: play mouse input (debug builds, see drive.rs).
    let mut drive_script: Option<String> = None;
    let mut write_sample: Option<usize> = None;
    let mut search = String::new();
    let mut mcp_port: Option<u16> = None;
    let mut rtls_test: Option<String> = None;
    let mut rtls_duration = 60.0;
    let mut rtls_report = String::from("rtls-report.json");
    let mut rtls_deploy = false;
    let mut sample: Option<usize> = None;
    let mut plant_test: Option<String> = None;
    let mut probe_ports: Vec<String> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--plant-test" => plant_test = args.next(),
            "--ports" => probe_ports.extend(args.next()),
            "--mcp-port" => mcp_port = args.next().and_then(|p| p.parse().ok()),
            "--no-mcp" => mcp_port = Some(0),
            "--layout" => layouts.extend(args.next()),
            "--screenshot" => shot = args.next(),
            "--export" => export = args.next(),
            "--place" => place.extend(args.next()),
            "--pose" => pose = true,
            "--preview" => preview = true,
            "--start" => keep_start = true,
            "--open" => open.extend(args.next()),
            "--no-select" => no_select = true,
            "--drive" => drive_script = args.next(),
            "--shot-delay" => shot_delay = args.next().and_then(|d| d.parse().ok()).unwrap_or(shot_delay),
            "--write-sample" => write_sample = args.next().and_then(|n| n.parse::<usize>().ok()).map(|n| n.saturating_sub(1)),
            "--search" => search = args.next().unwrap_or_default(),
            "--rtls-test" => rtls_test = args.next(),
            "--duration" => rtls_duration = args.next().and_then(|d| d.parse().ok()).unwrap_or(60.0),
            "--report" => rtls_report = args.next().unwrap_or(rtls_report),
            "--rtls-deploy" => rtls_deploy = true,
            "--sample" => sample = args.next().and_then(|n| n.parse::<usize>().ok()).map(|n| n.saturating_sub(1)),
            _ => root = Some(a),
        }
    }

    // `--ports ASSET.usd ...`: print each asset's role and ports as JSON (no window).
    if !probe_ports.is_empty() {
        for p in &probe_ports {
            let path = std::path::Path::new(p);
            let v = match prototype::probe(path) {
                Ok((mn, mx, det, joints)) => {
                    let info = plant::ports::resolve(path, mn, mx, &det);
                    let t = Transform::IDENTITY;
                    serde_json::json!({
                        "asset": p,
                        "role": info.role.name(),
                        "bounds_m": [mn.to_array(), mx.to_array()],
                        "joints": joints.iter().zip(&det.joints).map(|(j, g)| serde_json::json!({
                            "name": j, "radius_m": g.radius, "continuous": g.continuous,
                            "anchor": g.anchor.to_array(), "axis": g.axis.to_array(),
                        })).collect::<Vec<_>>(),
                        "rims": det.rims.iter().map(|r| serde_json::json!({ "center": r.center.to_array(), "normal": r.normal.to_array(), "radius_m": r.radius })).collect::<Vec<_>>(),
                        "ports": info.ports.iter().map(|q| plant::ports::port_json(q, &t)).collect::<Vec<_>>(),
                    })
                }
                Err(e) => serde_json::json!({ "asset": p, "error": format!("{e:#}") }),
            };
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        }
        return;
    }
    // `--write-sample N`: write sample N's layout file and print its path (no window).
    if let Some(i) = write_sample.filter(|&i| i < samples::SAMPLES.len()) {
        let lib = Library::new(library::default_root(root.clone()));
        match samples::write_layout(i, &lib) {
            Ok((path, missing)) => println!("{}{}", path.display(), if missing.is_empty() { String::new() } else { format!(" (not on disk: {})", missing.join(", ")) }),
            Err(e) => eprintln!("{e:#}"),
        }
        return;
    }
    // GPU choice (discrete / RTX first) and vsync from Settings → Graphics.
    let headless = shot.is_some() || rtls_test.is_some() || plant_test.is_some();
    let (gpu_prefs, gpu_recovered) = gpu::startup_prefs(headless);
    // Loading assets (texture decoding, mostly) leaves cores for drawing frames.
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    let _ = rayon::ThreadPoolBuilder::new().num_threads(cores.saturating_sub(2).max(2)).build_global();
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: APP_NAME.into(),
                    resolution: (1600u32, 950u32).into(),
                    present_mode: gpu_prefs.present_mode(),
                    // Scripted tests block for their whole run; no window to look frozen.
                    visible: rtls_test.is_none() && plant_test.is_none(),
                    ..default()
                }),
                ..default()
            })
            .set(bevy::render::RenderPlugin { render_creation: gpu_prefs.wgpu_settings().into(), ..default() }),
    )
    .add_plugins(gpu::GpuPlugin { prefs: gpu_prefs, recovered: gpu_recovered, headless })
    .add_plugins((EguiPlugin::default(), MeshPickingPlugin, FrameTimeDiagnosticsPlugin::default()))
    // Screenshot runs must not grab the MCP port of a running viewer.
    .add_plugins(agent::AgentPlugin { mcp_port: if shot.is_some() || rtls_test.is_some() || plant_test.is_some() { Some(0) } else { mcp_port } })
    .add_plugins(plant::PlantPlugin {
        autotest: plant_test.as_ref().map(|_| plant::AutoTest { duration: rtls_duration, report: rtls_report.clone().into(), frames_ready: 0 }),
    })
    .add_plugins(outcomes::OutcomesPlugin)
    .add_plugins(context_menu::InteractionPlugin)
    .add_plugins(tabs::TabsPlugin)
    .add_plugins(rtls::RtlsPlugin {
        autotest: rtls_test.as_ref().map(|_| rtls::sim::AutoTest { duration: rtls_duration as f64, report: rtls_report.clone().into(), frames_ready: 0, deploy: rtls_deploy }),
        headless: shot.is_some() || rtls_test.is_some() || plant_test.is_some(),
    })
    // A big layout's textures reach the GPU over several frames, not in one long one.
    .insert_resource(bevy::render::render_asset::RenderAssetBytesPerFrame::new(UPLOAD_BYTES_PER_FRAME))
    .init_gizmo_group::<HandleGizmos>()
    .insert_resource(Library::new(library::default_root(root)))
    .init_resource::<PrototypeCache>()
    .init_resource::<Selection>()
    .init_resource::<PlaceMode>()
    .init_resource::<DragState>()
    .init_resource::<HandleUi>()
    .init_resource::<PendingSpawns>()
    .init_resource::<scene::PendingDownloads>()
    .init_resource::<UiBlock>()
    .init_resource::<ViewSettings>()
    .init_resource::<Status>()
    .init_resource::<NameCounter>()
    .init_resource::<Actions>()
    .init_resource::<wall_tool::WallTool>()
    .init_resource::<ui::UiState>()
    .init_resource::<ui::HideOverlays>()
    .init_resource::<ui::LeftPanelSlot>()
    .add_systems(Startup, (setup_scene, setup_ui_camera))
    .add_systems(
        Update,
        (
            set_window_icon,
            prototype::receive_prototypes,
            update_ghost,
            process_pending,
            orbit_camera,
            wall_tool::draw_walls,
            interact.run_if(|w: Res<wall_tool::WallTool>| !w.active),
            keyboard_shortcuts.run_if(|w: Res<wall_tool::WallTool>| !w.active),
            apply_actions,
            apply_joints,
            gravity,
            sync_lights,
            sync_environment,
        )
            .chain(),
    )
    .init_resource::<lod::RenderStats>()
    .init_resource::<lod::Governor>()
    .add_systems(Update, (lod::sync_occlusion, lod::update_stats, lod::update_governor, lod::retry_new_meshes))
    .add_systems(
        PostUpdate,
        (
            draw_gizmos.after(TransformSystems::Propagate),
            lod::update_ranges.after(TransformSystems::Propagate).before(bevy::camera::visibility::VisibilitySystems::CheckVisibility),
        ),
    )
    .add_systems(EguiPrimaryContextPass, (ui::ui_system, ui::loading::loading_placeholders.after(ui::ui_system)))
    // Last in the pass, once every text field has drawn: IME on while one has focus.
    .add_systems(EguiPrimaryContextPass, ime::sync_ime.after(ui::ui_system).after(rtls::ui::rtls_ui));
    #[cfg(feature = "antenna-studio")]
    app.add_plugins(antenna::AntennaPlugin).add_systems(EguiPrimaryContextPass, antenna::ui::studio_ui.after(ui::ui_system).before(ime::sync_ime));

    app.world_mut().resource_mut::<Library>().search = search;
    if let Some(out) = shot {
        app.insert_resource(AutoShot { out: out.into(), frames_ready: 0, taken: false, export: export.map(Into::into), pose, preview, no_select, delay: shot_delay })
            .add_systems(Update, autoshot.after(apply_actions));
        let mut x = 0.0;
        for p in place {
            app.world_mut().resource_mut::<PendingSpawns>().0.push(PendingSpawn {
                asset: p.into(),
                transform: Transform::from_xyz(x, 0.0, 0.0),
                name: None,
                joints: None,
                select: false,
                locked: false,
            });
            x += 1.2;
        }
    }
    // `SENSESIM_LOG_FPS=1`: print frame rate and frame time every few seconds.
    if std::env::var_os("SENSESIM_LOG_FPS").is_some() {
        app.add_plugins(bevy::diagnostic::LogDiagnosticsPlugin { wait_duration: std::time::Duration::from_secs(3), ..default() });
    }
    if layouts.is_empty() {
        layouts.extend(rtls_test.or(plant_test));
    }
    // Scripted runs (layout, screenshot, sample) skip the start screen.
    if !keep_start && (!layouts.is_empty() || app.world().contains_resource::<AutoShot>() || sample.is_some()) {
        app.world_mut().resource_mut::<ui::UiState>().skip_start(sample);
    }
    let mut layouts = layouts.into_iter().map(std::path::PathBuf::from);
    if let Some(path) = layouts.next() {
        app.world_mut().resource_mut::<ui::UiState>().name_scene(tabs::title_of(&path));
        app.world_mut().resource_mut::<undo::UndoStack>().rebase();
        app.world_mut().resource_mut::<Actions>().0.push(Action::LoadLayout(path));
    }
    let more: Vec<tabs::NewTab> = layouts.map(|p| tabs::NewTab { title: tabs::title_of(&p), path: Some(p.clone()), load: Some(p) }).collect();
    if !more.is_empty() {
        app.world_mut().resource_mut::<tabs::Tabs>().request(tabs::Request::OpenInBackground(more));
    }
    app.world_mut().resource_mut::<ui::UiState>().open_at_start(open);
    #[cfg(debug_assertions)]
    if let Some(script) = &drive_script {
        app.insert_resource(drive::Drive::parse(script)).add_systems(PreUpdate, drive::play.before(bevy::input::InputSystems));
    }
    #[cfg(not(debug_assertions))]
    let _ = drive_script;
    app.run();
}

/// egui gets its own overlay camera so the 3D camera can be confined to the
/// viewport area between the panels.
fn setup_ui_camera(mut commands: Commands, mut egui_settings: ResMut<EguiGlobalSettings>) {
    egui_settings.auto_create_primary_context = false;
    commands.spawn((
        PrimaryEguiContext,
        Camera2d,
        RenderLayers::none(),
        Camera {
            order: 1,
            output_mode: CameraOutputMode::Write {
                blend_state: Some(BlendState::ALPHA_BLENDING),
                clear_color: ClearColorConfig::None,
            },
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
    ));
}

/// Puts the logo on the window / taskbar once the OS window exists.
fn set_window_icon(mut done: Local<bool>, window: Query<Entity, With<PrimaryWindow>>, _main_thread: NonSendMarker) {
    if *done {
        return;
    }
    let Ok(entity) = window.single() else { return };
    let Ok(img) = image::load_from_memory(include_bytes!("../assets/logo.png")) else {
        *done = true;
        return;
    };
    let img = img.to_rgba8();
    let (w, h) = img.dimensions();
    bevy::winit::WINIT_WINDOWS.with_borrow(|windows| {
        if let Some(win) = windows.get_window(entity)
            && let Ok(icon) = winit::window::Icon::from_rgba(img.into_raw(), w, h)
        {
            win.set_window_icon(Some(icon));
            *done = true;
        }
    });
}

/// `--screenshot out.png [--place a.usd ...] [--export scene.usda] [--pose] [--preview] [--no-select] [--open NAME ...]`:
/// places the given assets in a row, frames them, writes a screenshot once
/// everything is loaded, and exits. `--pose` moves every joint of the selected
/// object to 60% of its range. `--preview` hides the UI and saves only the 3D
/// view, cropped and scaled to a 960 × 540 picture (the sample previews).
/// `--shot-delay S` waits longer than the usual 8 s, e.g. to drive the window first.
/// Handy for checking rendering without clicking.
#[derive(Resource)]
struct AutoShot {
    out: std::path::PathBuf,
    frames_ready: u32,
    taken: bool,
    export: Option<std::path::PathBuf>,
    pose: bool,
    preview: bool,
    no_select: bool,
    delay: f32,
}

#[allow(clippy::too_many_arguments)]
fn autoshot(
    mut commands: Commands,
    mut shot: ResMut<AutoShot>,
    cache: Res<PrototypeCache>,
    pending: Res<PendingSpawns>,
    mut actions: ResMut<Actions>,
    mut objects: Query<(Entity, &mut PlacedObject)>,
    mut selection: ResMut<Selection>,
    mut exit: MessageWriter<AppExit>,
    mut cameras: Query<(&mut Camera, Has<PrimaryEguiContext>)>,
    mut orbit: Query<&mut OrbitCamera>,
    transforms: Query<&Transform>,
    time: Res<Time>,
) {
    if cache.loading_count() > 0 || !pending.0.is_empty() || time.elapsed_secs() < shot.delay {
        shot.frames_ready = 0;
        return;
    }
    shot.frames_ready += 1;
    if shot.frames_ready == 2 {
        actions.0.push(Action::FrameAll);
        if !shot.preview && !shot.no_select {
            selection.selected = objects.iter().next().map(|(e, _)| e);
        }
        if shot.pose
            && let Some((_, mut obj)) = objects.iter_mut().next()
        {
            let proto = obj.proto.clone();
            for (i, j) in proto.joints.iter().enumerate() {
                let (lo, hi) = j.range(proto.unit);
                obj.joints[i] = lo + (hi - lo) * 0.6;
            }
        }
        if let Some(p) = shot.export.clone() {
            actions.0.push(Action::SaveLayout(p.with_extension("json")));
            actions.0.push(Action::ExportUsda(p));
        }
    }
    // Preview: frame what the scene is about (not the floor and walls
    // around it) and fill the card with it.
    if shot.preview && shot.frames_ready == 6 {
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for (e, obj) in &objects {
            let Ok(t) = transforms.get(e) else { continue };
            if agent::structures::Kind::of_asset(&obj.asset).is_some() {
                continue;
            }
            let (mn, mx) = obj.aabb();
            for c in [mn, mx] {
                let p = t.transform_point(c);
                lo = lo.min(p);
                hi = hi.max(p);
            }
        }
        if lo.x <= hi.x {
            for mut o in &mut orbit {
                o.focus = (lo + hi) * 0.5;
                o.radius = ((hi - lo).length() * 0.95).max(1.5);
            }
        }
    }
    // Preview: stop drawing the UI; the 3D view keeps its place between the panels.
    if shot.preview && shot.frames_ready == 20 {
        for (mut cam, ui) in &mut cameras {
            if ui {
                cam.is_active = false;
            }
        }
    }
    if shot.frames_ready == 30 && !shot.taken {
        shot.taken = true;
        let out = shot.out.clone();
        if shot.preview {
            let vp = cameras.iter().find(|(_, ui)| !ui).and_then(|(c, _)| c.viewport.as_ref().map(|v| (v.physical_position, v.physical_size)));
            commands.spawn(bevy::render::view::screenshot::Screenshot::primary_window()).observe(
                move |ev: On<bevy::render::view::screenshot::ScreenshotCaptured>| {
                    let Ok(img) = ev.image.clone().try_into_dynamic() else { return };
                    let img = match vp {
                        Some((p, s)) => img.crop_imm(p.x, p.y, s.x, s.y),
                        None => img,
                    };
                    let img = img.resize_to_fill(960, 540, image::imageops::FilterType::Lanczos3);
                    if let Err(e) = img.to_rgb8().save(&out) {
                        error!("preview: {e}");
                    }
                },
            );
        } else {
            commands
                .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
                .observe(bevy::render::view::screenshot::save_to_disk(out));
        }
    }
    if shot.frames_ready == 45 {
        exit.write(AppExit::Success);
    }
}
