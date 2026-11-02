//! Built-in sample scenes offered on the start screen: a room shell (floor +
//! two walls, open towards the camera, plus interior walls and columns) with
//! dataset assets, generated or from a bundled layout file. Assets are given
//! by folder name, resolved against the current library root, and the result
//! is written out as an ordinary layout file.

use std::path::PathBuf;

use bevy::math::{Quat, Vec2, Vec3};

use crate::agent::structures::{self, Kind};
use crate::library::Library;
use crate::plant::recipes;
use crate::theme::icon;

#[path = "sample_scenes.rs"]
mod scenes;

/// (asset folder, x, z, yaw in degrees, y). `y` > 0 drops the object onto
/// what is under it (gravity).
pub type Item = (&'static str, f32, f32, f32, f32);

pub struct Sample {
    pub title: &'static str,
    pub blurb: &'static str,
    pub icon: &'static str,
    /// Asset whose thumbnail stands in when the preview image is missing.
    pub hero: &'static str,
    /// Rendered picture of the scene for its card (PNG; may be empty).
    pub preview: &'static [u8],
    /// Floor size (x, z) in meters around `center`; zero for no room shell.
    pub floor: Vec2,
    pub center: Vec2,
    pub wall_height: f32,
    /// Steel columns (x, z), as tall as the walls: what positioning devices
    /// in the open part of the room are mounted on.
    pub columns: &'static [(f32, f32)],
    /// Interior walls: (x, z, length, yaw in degrees, height).
    pub walls: &'static [(f32, f32, f32, f32, f32)],
    pub items: &'static [Item],
    /// Generated objects, added to `items`.
    pub generate: Option<fn() -> Vec<Item>>,
    /// A bundled layout file (assets given by folder name) used instead of
    /// `items`/`generate`; it may carry RTLS devices and zones. Set
    /// `SENSESIM_SAMPLE_RAW` to ignore it (to rebuild it from `generate`).
    pub layout: Option<&'static str>,
    /// A connected system built from generated parts (with its links and
    /// settings), added to the scene.
    pub plant: Option<fn() -> anyhow::Result<recipes::Assembly>>,
}

// The RTLS samples carry one technology each, so every link drawn is one
// that technology really measures.
pub const SAMPLES: [Sample; 9] = [
    Sample {
        title: "UWB warehouse",
        blurb: "40 × 28 m hall: 72 racks in six aisles, pallet staging and docks. UWB anchors on columns and racks track carts, robots and workers.",
        icon: icon::BROADCAST,
        hero: "rack_composition_a04",
        preview: include_bytes!("../assets/samples/previews/uwb_warehouse.png"),
        floor: Vec2::new(40.0, 28.0),
        center: Vec2::new(20.0, 14.0),
        wall_height: 6.0,
        columns: scenes::UWB_COLUMNS,
        walls: &[],
        items: &[],
        generate: Some(scenes::uwb_warehouse),
        layout: Some(include_str!("../assets/samples/uwb_warehouse.json")),
        plant: None,
    },
    Sample {
        title: "RFID warehouse",
        blurb: "30 × 20 m: 12 smart shelves with an antenna per level, tagged stock, a dock with pallets and portal readers.",
        icon: icon::BROADCAST,
        hero: "storage_shelf_basic_a01",
        preview: include_bytes!("../assets/samples/previews/rfid_warehouse.png"),
        floor: Vec2::new(30.0, 20.0),
        center: Vec2::new(15.0, 10.0),
        wall_height: 5.0,
        columns: scenes::RFID_COLUMNS,
        walls: &[],
        items: &[],
        generate: Some(scenes::rfid_warehouse),
        layout: Some(include_str!("../assets/samples/rfid_warehouse.json")),
        plant: None,
    },
    Sample {
        title: "Hospital ward",
        blurb: "Two six-bed wards off a corridor, a nurse station, supply room and family lounge.",
        icon: icon::PLUS,
        hero: "bed_hospital_standard_a01",
        preview: include_bytes!("../assets/samples/previews/hospital_ward.png"),
        floor: Vec2::new(24.0, 14.0),
        center: Vec2::ZERO,
        wall_height: 3.0,
        columns: &[],
        walls: scenes::HOSPITAL_WALLS,
        items: &[],
        generate: Some(scenes::hospital),
        layout: None,
        plant: None,
    },
    Sample {
        title: "Auto workshop",
        blurb: "Three service bays with car frames and engines on lift tables, a workbench line, tire storage and a compressor corner.",
        icon: icon::WRENCH,
        hero: "engineAssembly_v8_engine_a01",
        preview: include_bytes!("../assets/samples/previews/auto_workshop.png"),
        floor: Vec2::new(24.0, 16.0),
        center: Vec2::ZERO,
        wall_height: 5.0,
        columns: &[],
        walls: scenes::WORKSHOP_WALLS,
        items: &[],
        generate: Some(scenes::workshop),
        layout: None,
        plant: None,
    },
    Sample {
        title: "Office",
        blurb: "Open plan with 24 desks in four clusters, a meeting room, printer corner, lockers and a lounge.",
        icon: icon::SQUARES_FOUR,
        hero: "chair_office_fabric_manager_a01",
        preview: include_bytes!("../assets/samples/previews/office.png"),
        floor: Vec2::new(20.0, 14.0),
        center: Vec2::ZERO,
        wall_height: 3.0,
        columns: &[],
        walls: scenes::OFFICE_WALLS,
        items: &[],
        generate: Some(scenes::office),
        layout: None,
        plant: None,
    },
    Sample {
        title: "Canteen",
        blurb: "Kitchen line with fridges and coffee, islands, eight four-seat tables, drinks and recycling.",
        icon: icon::SUN,
        hero: "appliance_refrigerator_a01",
        preview: include_bytes!("../assets/samples/previews/canteen.png"),
        floor: Vec2::new(16.0, 12.0),
        center: Vec2::ZERO,
        wall_height: 3.0,
        columns: &[],
        walls: &[],
        items: &[],
        generate: Some(scenes::canteen),
        layout: None,
        plant: None,
    },
    Sample {
        title: "Pump station",
        blurb: "Two tanks, a motor pump and three valves, fully piped: the pump lifts water into tank B through a throttled discharge valve, and B drains back by gravity until the levels settle.",
        icon: icon::DROP,
        hero: "drainage_pumpSystem_a01",
        preview: include_bytes!("../assets/samples/previews/pump_station.png"),
        floor: Vec2::new(10.0, 7.0),
        center: Vec2::new(2.2, 0.3),
        wall_height: 4.0,
        columns: &[],
        walls: &[],
        items: &[],
        generate: None,
        layout: None,
        plant: Some(pump_station),
    },
    Sample {
        title: "Engine drive",
        blurb: "An inline-4 engine at 900 rpm: crank, pistons and rods move, a belt drives the coolant pump, and a generator turns on the flywheel.",
        icon: icon::GEAR,
        hero: "engineAssembly_v8_engine_a01",
        preview: include_bytes!("../assets/samples/previews/engine_drive.png"),
        floor: Vec2::new(6.0, 5.0),
        center: Vec2::new(0.6, 0.4),
        wall_height: 3.5,
        columns: &[],
        walls: &[],
        items: &[],
        generate: None,
        layout: None,
        plant: Some(engine_drive),
    },
    Sample {
        title: "Conveyor line",
        blurb: "Four roller conveyors linked end to end; boxes ride the line, transfer between conveyors and queue at the end.",
        icon: icon::ARROWS_CLOCKWISE,
        hero: "conveyor_belt_a02",
        preview: include_bytes!("../assets/samples/previews/conveyor_line.png"),
        floor: Vec2::new(11.0, 5.0),
        center: Vec2::new(3.0, 0.0),
        wall_height: 3.5,
        columns: &[],
        walls: &[],
        items: &[],
        generate: None,
        layout: None,
        plant: Some(conveyor_line),
    },
];

/// Small starting scenes the assistant loads with `load_template` and then
/// adapts, instead of placing everything one call at a time. Not shown on
/// the start screen.
pub const TEMPLATES: [(&str, Sample); 1] = [(
    "rack_warehouse",
    Sample {
        title: "Rack warehouse",
        blurb: "13 × 10 m room, 2 columns: 3 storage racks along the back wall, a workbench, a mobile cart. \
                UWB: 6 anchors (walls + columns), tag T1 on the cart, tags T2/T3 walking loops, zones Area 1-5 \
                (one per rack, workbench, cart bay).",
        icon: icon::BROADCAST,
        hero: "rack_composition_a04",
        preview: &[],
        floor: Vec2::new(13.0, 10.0),
        center: Vec2::new(6.5, 5.0),
        wall_height: 4.0,
        columns: &[(4.0, 4.5), (12.5, 4.5)],
        walls: &[],
        items: &[],
        generate: None,
        layout: Some(include_str!("../assets/templates/rack_warehouse.json")),
        plant: None,
    },
)];

fn pump_station() -> anyhow::Result<recipes::Assembly> {
    recipes::pump_loop(&mut recipes::Namer::new("", Default::default()), Vec3::ZERO, 0.0, 0.05, 1.0, 1.2, &[])
}

fn engine_drive() -> anyhow::Result<recipes::Assembly> {
    recipes::engine_drive(&mut recipes::Namer::new("", Default::default()), Vec3::ZERO, 0.0, 4, &[])
}

fn conveyor_line() -> anyhow::Result<recipes::Assembly> {
    recipes::conveyor_line(&mut recipes::Namer::new("", Default::default()), Vec3::new(-1.0, 0.0, 0.0), 0.0, 4, 2.0, 0.6, 0.8, 5, 0.4)
}

/// Asset folders a sample uses, one per placed object.
fn folders(sample: &Sample) -> Vec<String> {
    match bundled(sample) {
        Some(text) => {
            let v: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
            v["objects"].as_array().into_iter().flatten().filter_map(|o| o["asset"].as_str().map(str::to_string)).collect()
        }
        None => all_items(sample).iter().map(|it| it.0.to_string()).collect(),
    }
}

/// The bundled layout, unless `SENSESIM_SAMPLE_RAW` asks for the generated objects.
fn bundled(sample: &Sample) -> Option<&'static str> {
    sample.layout.filter(|_| std::env::var_os("SENSESIM_SAMPLE_RAW").is_none())
}

/// Listed plus generated objects.
fn all_items(sample: &Sample) -> Vec<Item> {
    let mut v = sample.items.to_vec();
    if let Some(g) = sample.generate {
        v.extend(g());
    }
    v
}

/// Objects in a sample (cached per sample; generating is cheap but the
/// start screen asks every frame).
pub fn object_count(index: usize) -> usize {
    static COUNTS: std::sync::OnceLock<Vec<usize>> = std::sync::OnceLock::new();
    COUNTS.get_or_init(|| SAMPLES.iter().map(|s| folders(s).len()).collect())[index]
}

/// RTLS devices bundled with a sample, by kind (`uwb_anchor`, `rfid_tag`…).
pub fn device_counts(index: usize) -> Vec<(String, usize)> {
    let Some(text) = SAMPLES[index].layout else { return Vec::new() };
    let v: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
    let mut out: Vec<(String, usize)> = Vec::new();
    for d in v["rtls"]["devices"].as_array().into_iter().flatten() {
        let k = d["kind"].as_str().unwrap_or("device").to_string();
        match out.iter_mut().find(|(n, _)| *n == k) {
            Some((_, c)) => *c += 1,
            None => out.push((k, 1)),
        }
    }
    out
}

/// How many of a sample's objects have their asset on disk, out of all.
pub fn available(sample: &Sample, library: &Library) -> (usize, usize) {
    let all = folders(sample);
    (all.iter().filter(|f| resolve(library, f).is_some()).count(), all.len())
}

/// The root USD of an asset folder, if it is on disk.
fn resolve(library: &Library, folder: &str) -> Option<PathBuf> {
    library.entries.iter().find(|e| e.folder == folder).and_then(|e| e.usd.clone())
}

/// Writes the sample as a layout file (`%LOCALAPPDATA%/SenseSim/samples/`)
/// and returns its path plus the folders that are not on disk. Their objects
/// stay in the layout as `"download": folder`, fetched when it is loaded.
pub fn write_layout(index: usize, library: &Library) -> anyhow::Result<(PathBuf, Vec<String>)> {
    write_scene(&SAMPLES[index], "samples", library)
}

/// Writes a template for the assistant (`%LOCALAPPDATA%/SenseSim/templates/`).
pub fn write_template(id: &str, library: &Library) -> anyhow::Result<(PathBuf, Vec<String>)> {
    let (_, s) = TEMPLATES.iter().find(|(i, _)| *i == id).ok_or_else(|| anyhow::anyhow!("unknown template '{id}'"))?;
    write_scene(s, "templates", library)
}

fn write_scene(s: &Sample, dir: &str, library: &Library) -> anyhow::Result<(PathBuf, Vec<String>)> {
    let mut missing: Vec<String> = Vec::new();
    let mut note_missing = |folder: &str| {
        if !missing.iter().any(|m| m == folder) {
            missing.push(folder.to_string());
        }
    };

    let mut objects = Vec::new();
    // `asset`: the USD on disk, or the online folder to download.
    let mut push = |name: Option<&str>, asset: Result<PathBuf, &str>, pos: Vec3, yaw: f32| {
        let mut o = serde_json::json!({
            "translation": pos.to_array(),
            "rotation": Quat::from_rotation_y(yaw.to_radians()).to_array(),
            "scale": [1.0, 1.0, 1.0],
            "joints": [],
        });
        match asset {
            Ok(usd) => o["asset"] = usd.to_string_lossy().into(),
            Err(folder) => o["download"] = folder.into(),
        }
        if let Some(n) = name {
            o["name"] = n.into();
        }
        objects.push(o);
    };

    // Floor slab, back wall (-z) and left wall (-x): the room stays open
    // towards the default camera. Columns stand in the open part.
    if s.floor != Vec2::ZERO {
        let (hx, hz) = (s.floor.x * 0.5, s.floor.y * 0.5);
        let at = |x: f32, y: f32, z: f32| Vec3::new(s.center.x + x, y, s.center.y + z);
        push(Some("floor"), Ok(structures::usd_for(Kind::Floor, Vec3::new(s.floor.x, 0.02, s.floor.y))?), at(0.0, -0.02, 0.0), 0.0);
        let wall = |len: f32| structures::usd_for(Kind::Wall, Vec3::new(len, s.wall_height, 0.2));
        push(Some("wall_back"), Ok(wall(s.floor.x)?), at(0.0, 0.0, -hz - 0.1), 0.0);
        push(Some("wall_left"), Ok(wall(s.floor.y + 0.2)?), at(-hx - 0.1, 0.0, 0.0), 90.0);
        for (i, &(x, z)) in s.columns.iter().enumerate() {
            let usd = structures::usd_for(Kind::Column, Vec3::new(0.4, s.wall_height, 0.4))?;
            push(Some(&format!("column_{}", i + 1)), Ok(usd), Vec3::new(x, 0.0, z), 0.0);
        }
        for (i, &(x, z, len, yaw, h)) in s.walls.iter().enumerate() {
            let usd = structures::usd_for(Kind::Wall, Vec3::new(len, h, 0.15))?;
            push(Some(&format!("wall_{}", i + 1)), Ok(usd), at(x, 0.0, z), yaw);
        }
    }

    let doc = if let Some(text) = bundled(s) {
        // Bundled layout: swap folder names for this library's USD paths.
        let mut doc: serde_json::Value = serde_json::from_str(text)?;
        for mut o in doc["objects"].as_array().cloned().unwrap_or_default() {
            let folder = o["asset"].as_str().unwrap_or_default().to_string();
            match resolve(library, &folder) {
                Some(usd) => {
                    o["asset"] = usd.to_string_lossy().into();
                    objects.push(o);
                }
                None => {
                    note_missing(&folder);
                    if let Some(m) = o.as_object_mut() {
                        m.remove("asset");
                    }
                    o["download"] = folder.into();
                    objects.push(o);
                }
            }
        }
        doc["objects"] = objects.into();
        doc
    } else {
        for (folder, x, z, yaw, y) in all_items(s) {
            match resolve(library, folder) {
                Some(usd) => push(None, Ok(usd), Vec3::new(x, y, z), yaw),
                None => {
                    note_missing(folder);
                    push(None, Err(folder), Vec3::new(x, y, z), yaw);
                }
            }
        }
        serde_json::json!({ "format": "sense_sim_layout", "version": 2, "objects": objects })
    };
    let mut doc = doc;
    if let Some(build) = s.plant {
        let a = build()?;
        let list = doc["objects"].as_array_mut().ok_or_else(|| anyhow::anyhow!("bad layout"))?;
        for it in &a.items {
            let lever = a.levers.iter().find(|(n, _)| *n == it.name).map(|(_, d)| vec![*d]).unwrap_or_default();
            list.push(serde_json::json!({
                "name": it.name,
                "asset": it.usd.to_string_lossy(),
                "translation": it.transform.translation.to_array(),
                "rotation": it.transform.rotation.to_array(),
                "scale": it.transform.scale.to_array(),
                "joints": lever,
            }));
        }
        let controls: std::collections::HashMap<String, crate::plant::sim::Control> = a.controls.into_iter().collect();
        doc["plant"] = serde_json::json!({ "links": a.links, "controls": controls, "running": true });
    }

    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    let dir = PathBuf::from(base).join("SenseSim").join(dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", slug(s.title)));
    std::fs::write(&path, serde_json::to_string_pretty(&doc)?)?;
    Ok((path, missing))
}

/// File-name friendly form of a scene name.
pub fn slug(name: &str) -> String {
    let s: String = name.trim().to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    if s.is_empty() { "scene".into() } else { s }
}
