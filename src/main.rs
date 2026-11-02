//! Sense Sim: browse NVIDIA SimReady USD assets, place them in a scene,
//! select/move/rotate them, play with their moving parts, and save or export
//! the layout.
//!
//! Usage: SenseSim [DATASET_ROOT] [--layout layout.json | --sample N]

// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod bvh;
mod context_menu;
#[cfg(feature = "design")]
mod design;
mod gpu;
mod ime;
mod library;
mod mcp;
mod plant;
mod prototype;
mod remote;
mod rtls;
mod samples;
mod scene;
#[cfg(feature = "design")]
mod sim;
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

/// File > Quit: with a design workspace, through its protection of unsaved work.
pub fn quit() {
    #[cfg(feature = "design")]
    design::request_quit();
    #[cfg(not(feature = "design"))]
    std::process::exit(0);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut root = None;
    let mut layout = None;
    let mut shot: Option<String> = None;
    let mut place: Vec<String> = Vec::new();
    let mut export: Option<String> = None;
    let mut pose = false;
    let mut preview = false;
    // `--start`: keep the start screen in a screenshot run.
    let mut keep_start = false;
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
    let mut design_test: Option<String> = None;
    let mut sim_test: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--design-test" => design_test = args.next(),
            "--sim-test" => sim_test = args.next(),
            "--sim-test-all" => {}
            "--plant-test" => plant_test = args.next(),
            "--ports" => probe_ports.extend(args.next()),
            "--mcp-port" => mcp_port = args.next().and_then(|p| p.parse().ok()),
            "--no-mcp" => mcp_port = Some(0),
            "--layout" => layout = args.next(),
            "--screenshot" => shot = args.next(),
            "--export" => export = args.next(),
            "--place" => place.extend(args.next()),
            "--pose" => pose = true,
            "--preview" => preview = true,
            "--start" => keep_start = true,
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
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: APP_NAME.into(),
                    resolution: (1600u32, 950u32).into(),
                    present_mode: gpu_prefs.present_mode(),
                    // Scripted tests block for their whole run; no window to look frozen.
                    // Nor one in front of whoever is using this desktop, for a long test.
                    visible: rtls_test.is_none() && plant_test.is_none() && std::env::var_os("SENSESIM_TEST_HIDDEN").is_none(),
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
    .add_plugins(context_menu::InteractionPlugin)
    .add_plugins(rtls::RtlsPlugin {
        autotest: rtls_test.as_ref().map(|_| rtls::sim::AutoTest { duration: rtls_duration as f64, report: rtls_report.clone().into(), frames_ready: 0, deploy: rtls_deploy }),
        headless: shot.is_some() || rtls_test.is_some() || plant_test.is_some(),
    })
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
    .add_systems(PostUpdate, draw_gizmos.after(TransformSystems::Propagate))
    .add_systems(EguiPrimaryContextPass, (ui::ui_system, ui::loading::loading_placeholders.after(ui::ui_system)))
    // Last in the pass, once every text field has drawn: IME on while one has focus.
    .add_systems(EguiPrimaryContextPass, ime::sync_ime.after(ui::ui_system).after(rtls::ui::rtls_ui));

    #[cfg(feature = "design")]
    {
        app.add_plugins((design::DesignPlugin, sim::SimPlugin));
        if let Some(out) = &sim_test {
            design::use_test_autosave();
            app.insert_resource(sim::autotest::SimTest::new(out.clone())).add_systems(Update, (sim::autotest::run.before(sim::persist::peek_actions), sim::autotest::tools_step));
            app.world_mut().resource_mut::<ui::UiState>().skip_start(None);
        }
        if let Some(out) = &design_test {
            design::use_test_autosave();
            app.insert_resource(design::autotest::DesignTest::new(out.clone())).add_systems(Update, design::autotest::run);
            app.world_mut().resource_mut::<ui::UiState>().skip_start(None);
        }
    }
    #[cfg(not(feature = "design"))]
    let _ = (&design_test, &sim_test);
    app.world_mut().resource_mut::<Library>().search = search;
    if let Some(out) = shot {
        app.insert_resource(AutoShot { out: out.into(), frames_ready: 0, taken: false, export: export.map(Into::into), pose, preview })
            .add_systems(Update, autoshot.after(apply_actions));
        let mut x = 0.0;
        for p in place {
            app.world_mut().resource_mut::<PendingSpawns>().0.push(PendingSpawn {
                asset: p.into(),
                transform: Transform::from_xyz(x, 0.0, 0.0),
                name: None,
                joints: None,
                select: false,
            });
            x += 1.2;
        }
    }
    // `SENSESIM_LOG_FPS=1`: print frame rate and frame time every few seconds.
    if std::env::var_os("SENSESIM_LOG_FPS").is_some() {
        app.add_plugins(bevy::diagnostic::LogDiagnosticsPlugin { wait_duration: std::time::Duration::from_secs(3), ..default() });
    }
    let layout = layout.or(rtls_test).or(plant_test);
    // Scripted runs (layout, screenshot, sample) skip the start screen.
    if !keep_start && (layout.is_some() || app.world().contains_resource::<AutoShot>() || sample.is_some()) {
        app.world_mut().resource_mut::<ui::UiState>().skip_start(sample);
    }
    if let Some(path) = layout {
        app.world_mut().resource_mut::<Actions>().0.push(Action::LoadLayout(path.into()));
    }
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

/// `--screenshot out.png [--place a.usd ...] [--export scene.usda] [--pose] [--preview]`:
/// places the given assets in a row, frames them, writes a screenshot once
/// everything is loaded, and exits. `--pose` moves every joint of the selected
/// object to 60% of its range. `--preview` hides the UI and saves only the 3D
/// view, cropped and scaled to a 480 × 270 card picture (the sample previews).
/// Handy for checking rendering without clicking.
#[derive(Resource)]
struct AutoShot {
    out: std::path::PathBuf,
    frames_ready: u32,
    taken: bool,
    export: Option<std::path::PathBuf>,
    pose: bool,
    preview: bool,
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
    if cache.loading_count() > 0 || !pending.0.is_empty() || time.elapsed_secs() < 8.0 {
        shot.frames_ready = 0;
        return;
    }
    shot.frames_ready += 1;
    if shot.frames_ready == 2 {
        actions.0.push(Action::FrameAll);
        if !shot.preview {
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
                    let img = img.resize_to_fill(480, 270, image::imageops::FilterType::Lanczos3);
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
