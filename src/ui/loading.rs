//! Placeholders for objects that are not in the scene yet: a translucent grey
//! box of the object's size where it will appear, labelled "Loading…" while
//! the USD is read, or with the download progress while its asset folder is
//! fetched from the online library (samples whose assets are not on disk).

use bevy::prelude::*;
use bevy_egui::{EguiContexts, egui};
use egui::{Color32, vec2};

use crate::agent::catalog::Catalog;
use crate::library::{DownloadState, Library};
use crate::prototype::{ProtoState, PrototypeCache};
use crate::scene::{OrbitCamera, PendingDownloads, PendingSpawn, PendingSpawns, Status};
use crate::theme;

/// A placeholder box; the key identifies the object it stands for.
#[derive(Component)]
pub struct LoadingBox(String);

/// Size when nothing is known about the asset yet.
const UNKNOWN_SIZE: Vec3 = Vec3::new(0.8, 1.0, 0.8);

struct Wanted {
    key: String,
    transform: Transform,
    size: Vec3,
    label: String,
}

#[allow(clippy::too_many_arguments)]
pub fn loading_placeholders(
    mut commands: Commands,
    mut contexts: EguiContexts,
    mut library: ResMut<Library>,
    mut pending: ResMut<PendingSpawns>,
    mut downloads: ResMut<PendingDownloads>,
    mut status: ResMut<Status>,
    (cache, catalog, time): (Res<PrototypeCache>, Res<Catalog>, Res<Time>),
    boxes: Query<(Entity, &LoadingBox)>,
    camera: Single<(&Camera, &GlobalTransform), With<OrbitCamera>>,
    (mut meshes, mut materials): (ResMut<Assets<Mesh>>, ResMut<Assets<StandardMaterial>>),
    mut look: Local<Option<(Handle<Mesh>, Handle<StandardMaterial>)>>,
) -> Result {
    // Downloads: queue them, and hand finished ones over as normal spawns.
    let mut failed = Vec::new();
    downloads.0.retain_mut(|d| {
        if let Some(usd) = library.entries.iter().find(|e| e.folder == d.folder).and_then(|e| e.usd.clone()) {
            pending.0.push(PendingSpawn { asset: usd, transform: d.transform, name: d.name.take(), joints: None, select: false });
            return false;
        }
        if !d.requested {
            library.start_download(&d.folder, false);
            d.requested = true;
        } else if library.download_state(&d.folder).is_none() {
            failed.push(d.folder.clone());
            return false;
        }
        true
    });
    if let Some(f) = failed.first() {
        status.0 = format!("Could not download {f} - the object was left out");
    }

    let dots = ".".repeat(1 + (time.elapsed_secs() * 2.0) as usize % 3);
    let mut wanted: Vec<Wanted> = Vec::new();
    for p in &pending.0 {
        if matches!(cache.states.get(&p.asset), Some(ProtoState::Failed(_))) {
            continue;
        }
        let size = catalog.get(&p.asset).map(Vec3::from).unwrap_or(UNKNOWN_SIZE);
        let name = p.asset.file_stem().map(|s| s.to_string_lossy().trim_start_matches("sm_").to_string()).unwrap_or_default();
        wanted.push(Wanted { key: format!("{}@{:?}", p.asset.display(), p.transform.translation), transform: p.transform, size, label: format!("Loading {name}{dots}") });
    }
    for d in &downloads.0 {
        // Measured earlier (the catalog keeps sizes of assets seen before)?
        let size = catalog
            .dims
            .iter()
            .find(|(k, _)| k.replace('\\', "/").contains(&format!("/{}/", d.folder)))
            .map(|(_, s)| Vec3::from(*s))
            .unwrap_or(UNKNOWN_SIZE);
        let state = match library.download_state(&d.folder) {
            Some(DownloadState::Active { done, total }) if total > 0 => format!("Downloading {:.0}%", done as f64 * 100.0 / total as f64),
            Some(DownloadState::Active { .. }) => format!("Downloading{dots}"),
            _ => "Waiting to download".into(),
        };
        wanted.push(Wanted { key: format!("dl:{}@{:?}", d.folder, d.transform.translation), transform: d.transform, size, label: format!("{state} · {}", d.folder) });
    }

    // Sync the box entities.
    for (e, b) in &boxes {
        if !wanted.iter().any(|w| w.key == b.0) {
            commands.entity(e).despawn();
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }
    let (mesh, mat) = look
        .get_or_insert_with(|| {
            let mat = StandardMaterial {
                base_color: Color::srgba(0.62, 0.64, 0.67, 0.4),
                alpha_mode: AlphaMode::Blend,
                perceptual_roughness: 0.9,
                ..default()
            };
            (meshes.add(Cuboid::new(1.0, 1.0, 1.0)), materials.add(mat))
        })
        .clone();
    // Breathe, so it reads as "in progress".
    if let Some(mut m) = materials.get_mut(&mat) {
        m.base_color.set_alpha(0.3 + 0.15 * (time.elapsed_secs() * 3.0).sin());
    }
    let box_transform = |w: &Wanted| {
        let size = w.size.max(Vec3::splat(0.05));
        Transform {
            // Assets are bottom-centred on their origin.
            translation: w.transform.transform_point(Vec3::Y * size.y * 0.5),
            rotation: w.transform.rotation,
            scale: w.transform.scale * size,
        }
    };
    for w in &wanted {
        if !boxes.iter().any(|(_, b)| b.0 == w.key) {
            commands.spawn((
                LoadingBox(w.key.clone()),
                Mesh3d(mesh.clone()),
                MeshMaterial3d(mat.clone()),
                box_transform(w),
                bevy::picking::Pickable::IGNORE,
                Name::new("loading placeholder"),
            ));
        }
    }

    // Labels on top of the boxes.
    let ctx = contexts.ctx_mut()?;
    let (cam, cam_gt) = *camera;
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("loading_labels")));
    for w in &wanted {
        let top = w.transform.transform_point(Vec3::Y * w.size.y.max(0.05));
        let Ok(s) = cam.world_to_viewport(cam_gt, top) else { continue };
        let galley = painter.layout_no_wrap(w.label.clone(), egui::FontId::proportional(11.0), Color32::from_gray(235));
        let r = egui::Rect::from_center_size(egui::pos2(s.x, s.y - 12.0), galley.size() + vec2(10.0, 4.0));
        painter.rect_filled(r, 4.0, Color32::from_black_alpha(170));
        painter.galley(r.min + vec2(5.0, 2.0), galley, theme::TEXT);
    }
    Ok(())
}
