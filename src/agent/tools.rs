//! The tools the assistant (and MCP clients) can call, and the bridge that runs
//! them on the Bevy world. Worker threads send a [`ToolJob`]; the exclusive
//! system [`run_tool_jobs`] executes it, parking calls that need to wait (asset
//! loads, downloads, screenshots, user answers) until they can finish.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::ecs::system::SystemState;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::{Value, json};

use super::catalog::{self, Catalog};
use super::llm::{Block, ToolDef};
use super::{Agent, ChatItem, Question, images};
use crate::library::Library;
use crate::prototype::{ProtoState, PrototypeCache};
use crate::scene::{Action, Actions, NameCounter, OrbitCamera, PlacedObject, spawn_object};

// ---------------------------------------------------------------------------
// Bridge

pub struct ToolOutput {
    pub content: Vec<Block>,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(v: Value) -> Self {
        Self { content: vec![Block::Text(pretty(&v))], is_error: false }
    }
    pub fn text(s: impl Into<String>) -> Self {
        Self { content: vec![Block::Text(s.into())], is_error: false }
    }
    pub fn err(s: impl Into<String>) -> Self {
        Self { content: vec![Block::Text(s.into())], is_error: true }
    }
    pub fn summary(&self) -> String {
        self.content
            .iter()
            .map(|b| match b {
                Block::Text(t) => t.clone(),
                Block::Image { .. } => "[image]".into(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

pub struct ToolJob {
    pub name: String,
    pub input: Value,
    pub reply: Sender<ToolOutput>,
    /// Shown in the assistant transcript (calls from MCP clients).
    pub from_mcp: bool,
}

#[derive(Resource, Clone)]
pub struct ToolBridge {
    pub tx: Sender<ToolJob>,
    rx: Receiver<ToolJob>,
}

impl Default for ToolBridge {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Self { tx, rx }
    }
}

/// Blocking call from a worker thread. Returns early if `cancel` is set.
pub fn call(tx: &Sender<ToolJob>, name: &str, input: Value, from_mcp: bool, cancel: Option<&AtomicBool>) -> ToolOutput {
    let (rtx, rrx) = unbounded();
    if tx.send(ToolJob { name: name.into(), input, reply: rtx, from_mcp }).is_err() {
        return ToolOutput::err("viewer is shutting down");
    }
    loop {
        match rrx.recv_timeout(Duration::from_millis(200)) {
            Ok(out) => return out,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return ToolOutput::err("tool call was dropped"),
            Err(_) if cancel.map(|c| c.load(Ordering::Relaxed)).unwrap_or(false) => {
                return ToolOutput::err("cancelled by the user");
            }
            Err(_) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Definitions

fn def(name: &str, description: &str, schema: Value) -> ToolDef {
    ToolDef { name: name.into(), description: description.into(), schema }
}

/// Tools for the in-app assistant. `mcp` adds the tools only external clients need.
pub fn tool_defs(mcp: bool) -> Vec<ToolDef> {
    let vec3 = json!({ "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 });
    let mut v = vec![
        def(
            "list_asset_categories",
            "List every kind of SimReady asset in the library (names without variant suffix) with how many variants exist. Use it to learn the vocabulary before searching.",
            json!({ "type": "object", "properties": {} }),
        ),
        def(
            "search_assets",
            "Search the SimReady library by keywords (e.g. 'pallet', 'rack shelf', 'cardboard box'). Returns matching assets with on_disk status and measured size_m [width, height, depth] when known. Pass size_m of the real object to rank by size. show_thumbnails returns preview images of the top results.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "description": "default 12" },
                    "size_m": { "type": "array", "items": { "type": "number" }, "description": "target [width, height, depth] in m; 0 = unknown" },
                    "on_disk_only": { "type": "boolean" },
                    "show_thumbnails": { "type": "boolean", "description": "attach up to 6 thumbnails" }
                },
                "required": ["query"]
            }),
        ),
        def(
            "get_asset_info",
            "Load an asset and report its exact size in meters, triangle count and joints; optionally return its thumbnail.",
            json!({
                "type": "object",
                "properties": { "asset": { "type": "string" }, "include_thumbnail": { "type": "boolean" } },
                "required": ["asset"]
            }),
        ),
        def(
            "download_assets",
            "Download online-only assets (search results with on_disk=false) by folder name. Waits until done and returns their USD paths.",
            json!({
                "type": "object",
                "properties": { "folders": { "type": "array", "items": { "type": "string" } } },
                "required": ["folders"]
            }),
        ),
        def(
            "place_objects",
            "Place assets in the scene. Coordinates: meters, Y up, floor at y=0; an asset's origin is the bottom-center of its bounding box. x/z are the floor position; y defaults to 0 (or the top of `on_top_of`). yaw_deg rotates around Y (0 = asset's default facing, +Z). Returns the created object names.",
            json!({
                "type": "object",
                "properties": {
                    "objects": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "asset": { "type": "string", "description": "asset path or library name from search_assets" },
                                "x": { "type": "number" }, "z": { "type": "number" }, "y": { "type": "number" },
                                "yaw_deg": { "type": "number" },
                                "scale": { "type": "number", "description": "uniform scale, default 1; avoid unless the size is clearly different" },
                                "on_top_of": { "type": "string", "description": "name of a placed object to stack on" },
                                "connect_to": {
                                    "type": "object",
                                    "description": "Snap this object so one of its ports mates with a port of a placed object (x/z/yaw are then computed; see plant_list_ports). Upright equipment only turns about Y; generated piping turns freely.",
                                    "properties": {
                                        "object": { "type": "string" }, "port": { "type": "string" },
                                        "my_port": { "type": "string", "description": "this asset's port; default: the first free one that mates" },
                                        "aim": { "type": "array", "items": { "type": "number" }, "description": "piping: direction [x,y,z] the part's other port should face" }
                                    },
                                    "required": ["object", "port"]
                                },
                                "name": { "type": "string" }
                            },
                            "required": ["asset", "x", "z"]
                        }
                    }
                },
                "required": ["objects"]
            }),
        ),
        def(
            "get_scene",
            "List placed objects with name, asset, position, yaw, scale and world size.",
            json!({ "type": "object", "properties": {} }),
        ),
        def(
            "update_object",
            "Move, rotate, rescale or rename a placed object. Omitted fields stay unchanged.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "x": { "type": "number" }, "y": { "type": "number" }, "z": { "type": "number" },
                    "yaw_deg": { "type": "number" }, "scale": { "type": "number" },
                    "new_name": { "type": "string" }
                },
                "required": ["name"]
            }),
        ),
        def(
            "delete_objects",
            "Delete placed objects by name.",
            json!({ "type": "object", "properties": { "names": { "type": "array", "items": { "type": "string" } } }, "required": ["names"] }),
        ),
        def("clear_scene", "Remove every placed object.", json!({ "type": "object", "properties": {} })),
        def(
            "verify_scene",
            "Measure the scene for physical problems: objects floating above or sunk into what is below them, and solid objects intersecting each other. Call it after placing or moving things and fix every issue it reports; the harness also runs it automatically before you finish.",
            json!({ "type": "object", "properties": {} }),
        ),
        def(
            "update_plan",
            "Show your step-by-step plan to the user as a checklist. Call it at the start of multi-step work and whenever a step's status changes.",
            json!({
                "type": "object",
                "properties": {
                    "steps": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "step": { "type": "string" },
                                "status": { "type": "string", "enum": ["pending", "in_progress", "done", "failed"] }
                            },
                            "required": ["step", "status"]
                        }
                    }
                },
                "required": ["steps"]
            }),
        ),
        def(
            "set_camera",
            "Point the viewer camera: eye and target positions in meters, optional horizontal field of view. Use the camera from estimate_depth to match a reference photo's viewpoint.",
            json!({
                "type": "object",
                "properties": { "eye": vec3, "target": vec3, "hfov_deg": { "type": "number" } },
                "required": ["eye", "target"]
            }),
        ),
        def(
            "frame_all",
            "Move the camera so every placed object is visible.",
            json!({ "type": "object", "properties": {} }),
        ),
        def(
            "screenshot",
            "Capture the 3D viewport as an image, to check the layout or compare it with a reference photo.",
            json!({ "type": "object", "properties": {} }),
        ),
        def(
            "ask_user",
            "Ask the user a question and wait for the answer. Use for missing scale/dimensions, choosing between candidate assets (candidate_assets shows clickable thumbnails), what to do when no matching asset exists, and whether to refine after a review. Keep questions short; offer options when possible.",
            json!({
                "type": "object",
                "properties": {
                    "question": { "type": "string" },
                    "options": { "type": "array", "items": { "type": "string" } },
                    "candidate_assets": { "type": "array", "items": { "type": "string" } },
                    "allow_text": { "type": "boolean", "description": "allow a free-text answer, default true" }
                },
                "required": ["question"]
            }),
        ),
        def(
            "perceive_photo",
            "Measure a reference photo with on-device vision models (metric depth + open-vocabulary detection): recovers the photo camera pose, finds every instance of the given labels, and measures each object's floor position, width, height and orientation in meters, plus walls and the visible floor area. Returns numbered detections and an overlay image (green numbered boxes). Labels should be short concrete nouns the detector knows, e.g. [\"pallet rack\", \"pallet\", \"cardboard box\", \"forklift\", \"column\", \"conveyor\", \"workbench\"]. Add extra_boxes for objects the detector misses. Re-run with known_size once you know a real dimension.",
            json!({
                "type": "object",
                "properties": {
                    "image_id": { "type": "string" },
                    "labels": { "type": "array", "items": { "type": "string" } },
                    "extra_boxes": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": { "type": "string" },
                                "x0": { "type": "number" }, "y0": { "type": "number" },
                                "x1": { "type": "number" }, "y1": { "type": "number" }
                            },
                            "required": ["label", "x0", "y0", "x1", "y1"]
                        },
                        "description": "normalized [0,1] boxes (top-left x0,y0 to bottom-right x1,y1) reaching down to where the object touches the floor"
                    },
                    "hfov_deg": { "type": "number", "description": "horizontal field of view if known; otherwise EXIF or the configured default is used" },
                    "known_size": {
                        "type": "object",
                        "properties": { "id": { "type": "integer" }, "width_m": { "type": "number" }, "height_m": { "type": "number" } },
                        "required": ["id"],
                        "description": "a detection whose real width or height you know; rescales all distances"
                    },
                    "threshold": { "type": "number", "description": "detection confidence, default 0.2" }
                },
                "required": ["image_id", "labels"]
            }),
        ),
        def(
            "place_from_photo",
            "Place assets for detections from perceive_photo. Position, orientation and (only when clearly needed) scale are computed from the measurement and the asset's real size - do not pass coordinates. Give yaw_deg only to override a wrong orientation. List detections that should not become objects in skip.",
            json!({
                "type": "object",
                "properties": {
                    "image_id": { "type": "string" },
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "integer", "description": "detection id" },
                                "asset": { "type": "string" },
                                "yaw_deg": { "type": "number" },
                                "name": { "type": "string" }
                            },
                            "required": ["id", "asset"]
                        }
                    },
                    "skip": { "type": "array", "items": { "type": "integer" } }
                },
                "required": ["image_id", "items"]
            }),
        ),
        def(
            "optimize_layout",
            "Nudge objects placed from a photo (position, yaw, scale within limits) so their outlines line up with the detections in the photo, without creating overlaps. Returns alignment before/after. Run after place_from_photo, then verify_scene.",
            json!({ "type": "object", "properties": { "image_id": { "type": "string" } }, "required": ["image_id"] }),
        ),
        def(
            "add_structure",
            "Add building structure that isn't in the asset library: floor slab, walls, columns, ceiling, roof beams or plain blocks. Either give one piece (kind, size_m = [length, height, depth], x, z, yaw_deg), or from_photo = image_id to build the room shell measured by perceive_photo (visible floor, fitted walls, detected columns, ceiling). A ceiling without size_m covers the whole scene at height_m (default: tallest object + 1 m); with_beams adds roof trusses under it. Ceilings are see-through from above and are the main mounting surface for RTLS anchors.",
            json!({
                "type": "object",
                "properties": {
                    "from_photo": { "type": "string" },
                    "kind": { "type": "string", "enum": ["floor", "wall", "column", "block", "ceiling", "roof", "beam"] },
                    "size_m": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                    "x": { "type": "number" }, "z": { "type": "number" }, "y": { "type": "number" },
                    "height_m": { "type": "number", "description": "ceiling height" },
                    "with_beams": { "type": "boolean" },
                    "beam_spacing_m": { "type": "number", "description": "default 6" },
                    "yaw_deg": { "type": "number" },
                    "name": { "type": "string" }
                }
            }),
        ),
        def(
            "save_layout",
            "Save the scene as a layout JSON file.",
            json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
        ),
        def(
            "export_usda",
            "Export the scene as a Y-up USDA stage referencing the original assets (opens in Isaac Sim / Omniverse / usdview).",
            json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
        ),
    ];
    if mcp {
        v.push(def(
            "load_reference_image",
            "Load an image file (a photo of the scene to rebuild) and return its image_id for estimate_depth, plus the image itself.",
            json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
        ));
    }
    v.extend(crate::rtls::tools::defs());
    v.extend(crate::plant::tools::defs());
    v
}

// ---------------------------------------------------------------------------
// Execution

const LOAD_TIMEOUT: Duration = Duration::from_secs(240);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(1800);

#[derive(Default)]
pub(crate) struct PlaceItem {
    pub usd: PathBuf,
    pub transform: Transform,
    pub name: Option<String>,
    pub on_top_of: Option<String>,
    /// (image id, detection id, yaw override): transform computed from the
    /// photo measurement once the asset size is known.
    pub photo: Option<(String, usize, Option<f32>)>,
    /// Snap one of this object's ports onto a port of a placed object.
    pub connect: Option<ConnectTo>,
    /// Links to register once the object exists (precomputed assemblies).
    pub links: Vec<crate::plant::connect::Link>,
}

pub(crate) struct ConnectTo {
    pub target: crate::plant::connect::PortRef,
    /// This object's port; the first free one that mates when `None`.
    pub my_port: Option<String>,
    /// Where the part's other port should point (piping only).
    pub aim: Option<Vec3>,
}

/// What a tool module asks the bridge to do.
pub(crate) enum Started {
    Done(ToolOutput),
    /// Spawn these (waiting for their assets), then reply with what was placed plus `extra`.
    Place { items: Vec<PlaceItem>, extra: Value },
    /// Load these assets first, then continue.
    After { usds: Vec<PathBuf>, then: Box<dyn FnOnce(&mut World) -> Started + Send + Sync> },
}

/// Runs a tool module's answer: replies now, or parks it.
fn run_started(world: &mut World, s: Started, reply: Sender<ToolOutput>) -> Option<ToolOutput> {
    match s {
        Started::Done(out) => Some(out),
        Started::Place { items, extra } => {
            let mut cache = world.resource_mut::<PrototypeCache>();
            for it in &items {
                cache.request(&it.usd);
            }
            defer(world, Wait::Place { items, reply, started: Instant::now(), extra })
        }
        Started::After { usds, then } => {
            let mut cache = world.resource_mut::<PrototypeCache>();
            for u in &usds {
                cache.request(u);
            }
            defer(world, Wait::After { usds, then, reply, started: Instant::now() })
        }
    }
}

enum Wait {
    Place { items: Vec<PlaceItem>, reply: Sender<ToolOutput>, started: Instant, extra: Value },
    After { usds: Vec<PathBuf>, then: Box<dyn FnOnce(&mut World) -> Started + Send + Sync>, reply: Sender<ToolOutput>, started: Instant },
    AssetInfo { usd: PathBuf, thumb: bool, reply: Sender<ToolOutput>, started: Instant },
    /// `slot` is `None` until loads settle and the capture is requested.
    Screenshot { slot: Option<Arc<Mutex<Option<Image>>>>, settled: u32, reply: Sender<ToolOutput>, started: Instant },
    Download { folders: Vec<String>, reply: Sender<ToolOutput>, started: Instant },
}

#[derive(Resource, Default)]
pub struct PendingTools(Vec<Wait>, Option<Instant>);

pub fn run_tool_jobs(world: &mut World) {
    let jobs: Vec<ToolJob> = world.resource::<ToolBridge>().rx.try_iter().collect();
    for job in jobs {
        if job.from_mcp {
            let summary = format!("{} {}", job.name, short(&job.input.to_string(), 160));
            world.resource_mut::<Agent>().transcript.push(ChatItem::Info(format!("MCP client → {summary}")));
        }
        let reply = job.reply.clone();
        if let Some(out) = start(world, job) {
            let _ = reply.send(out);
        }
    }
    let waits = std::mem::take(&mut world.resource_mut::<PendingTools>().0);
    let mut keep = Vec::new();
    for w in waits {
        if let Some(w) = poll(world, w) {
            keep.push(w);
        }
    }
    world.resource_mut::<PendingTools>().0.extend(keep);
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() > n { format!("{}…", s.chars().take(n).collect::<String>()) } else { s.to_string() }
}

fn f(v: &Value, k: &str) -> Option<f32> {
    v[k].as_f64().map(|x| x as f32)
}

pub(crate) fn vec3(v: &Value) -> Option<Vec3> {
    let a = v.as_array()?;
    if a.len() != 3 {
        return None;
    }
    Some(Vec3::new(a[0].as_f64()? as f32, a[1].as_f64()? as f32, a[2].as_f64()? as f32))
}

/// Finds the USD for an asset reference: a path, or a library name/folder.
pub(crate) fn resolve_asset(library: &Library, s: &str) -> Result<PathBuf, String> {
    let p = Path::new(s);
    if p.is_file() {
        return Ok(p.to_path_buf());
    }
    let norm = s.replace('\\', "/").to_ascii_lowercase();
    let e = library
        .entries
        .iter()
        .find(|e| e.usd.as_ref().map(|u| u.to_string_lossy().replace('\\', "/").to_ascii_lowercase() == norm).unwrap_or(false))
        .or_else(|| library.entries.iter().find(|e| e.name.eq_ignore_ascii_case(s)))
        .or_else(|| library.entries.iter().find(|e| e.folder.eq_ignore_ascii_case(s)));
    match e {
        Some(e) => e.usd.clone().ok_or_else(|| format!("'{s}' is not on disk yet - call download_assets with folder '{}'", e.folder)),
        None => Err(format!("unknown asset '{s}' - use search_assets to find valid names")),
    }
}

pub(crate) fn find_object(world: &mut World, name: &str) -> Option<Entity> {
    let mut q = world.query::<(Entity, &PlacedObject)>();
    q.iter(world).find(|(_, o)| o.name == name).map(|(e, _)| e)
}

/// World-space bounds of a placed object.
pub(crate) fn world_bounds(o: &PlacedObject, t: &Transform) -> (Vec3, Vec3) {
    let (a, b) = (o.proto.aabb_min, o.proto.aabb_max);
    let mut mn = Vec3::splat(f32::MAX);
    let mut mx = Vec3::splat(f32::MIN);
    for x in [a.x, b.x] {
        for y in [a.y, b.y] {
            for z in [a.z, b.z] {
                let p = t.transform_point(Vec3::new(x, y, z));
                mn = mn.min(p);
                mx = mx.max(p);
            }
        }
    }
    (mn, mx)
}

pub(crate) fn r2(v: f32) -> f64 {
    (v as f64 * 100.0).round() / 100.0
}

pub(crate) fn object_json(o: &PlacedObject, t: &Transform) -> Value {
    let (mn, mx) = world_bounds(o, t);
    let (yaw, _, _) = t.rotation.to_euler(EulerRot::YXZ);
    json!({
        "name": o.name,
        "asset": o.asset.to_string_lossy().replace('\\', "/"),
        "position": [r2(t.translation.x), r2(t.translation.y), r2(t.translation.z)],
        "yaw_deg": r2(yaw.to_degrees()),
        "scale": r2(t.scale.x),
        "world_size_m": [r2(mx.x - mn.x), r2(mx.y - mn.y), r2(mx.z - mn.z)],
    })
}

/// Thumbnail of a library asset as a small PNG, from disk.
pub fn thumbnail_png(usd: Option<&Path>, folder: &str) -> Option<Vec<u8>> {
    let mut candidates = Vec::new();
    if let Some(u) = usd
        && let Some(dir) = u.parent()
    {
        let file = u.file_name()?.to_string_lossy().to_string();
        let stem = u.file_stem()?.to_string_lossy().to_string();
        candidates.push(dir.join(".thumbs/256x256").join(format!("{file}.png")));
        candidates.push(dir.join(".thumbs").join(format!("{stem}_thumbnail.png")));
    }
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    candidates.push(Path::new(&base).join("SenseSim").join("thumbs").join(format!("{folder}.png")));
    let path = candidates.into_iter().find(|p| p.exists())?;
    let img = image::open(path).ok()?.thumbnail(256, 256);
    encode_png(&img)
}

fn encode_png(img: &image::DynamicImage) -> Option<Vec<u8>> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

fn png_block(data: Vec<u8>) -> Block {
    Block::Image { data: Arc::new(data), mime: "image/png".into() }
}

/// Starts a job. Returns the output when it finished immediately.
fn start(world: &mut World, job: ToolJob) -> Option<ToolOutput> {
    let i = &job.input;
    let reply = job.reply.clone();
    let out = match job.name.as_str() {
        "list_asset_categories" => {
            let lib = world.resource::<Library>();
            let cats = catalog::categories(lib);
            let list: Vec<String> = cats.iter().map(|(k, n)| if *n > 1 { format!("{k} ({n})") } else { k.clone() }).collect();
            ToolOutput::text(format!("{} kinds of assets:\n{}", list.len(), list.join(", ")))
        }
        "search_assets" => {
            let query = i["query"].as_str().unwrap_or("");
            let limit = i["limit"].as_u64().unwrap_or(12).clamp(1, 50) as usize;
            let mut size = [0.0; 3];
            for (k, v) in i["size_m"].as_array().into_iter().flatten().take(3).enumerate() {
                size[k] = v.as_f64().unwrap_or(0.0) as f32;
            }
            let lib = world.resource::<Library>();
            let cat = world.resource::<Catalog>();
            let mut hits = catalog::search(lib, cat, query, size, limit * 3);
            if i["on_disk_only"].as_bool().unwrap_or(false) {
                hits.retain(|h| lib.entries[h.index].usd.is_some());
            }
            hits.truncate(limit);
            let list: Vec<Value> = hits.iter().map(|h| catalog::entry_json(lib, cat, h.index)).collect();
            let mut out = ToolOutput::ok(json!({ "query": query, "results": list }));
            if list.is_empty() {
                out = ToolOutput::text(format!("No assets match '{query}'. Try other words or list_asset_categories."));
            } else if i["show_thumbnails"].as_bool().unwrap_or(false) {
                for h in hits.iter().take(6) {
                    let e = &lib.entries[h.index];
                    if let Some(png) = thumbnail_png(e.usd.as_deref(), &e.folder) {
                        out.content.push(Block::Text(format!("thumbnail of {}:", e.name)));
                        out.content.push(png_block(png));
                    }
                }
            }
            out
        }
        "get_asset_info" => {
            let lib = world.resource::<Library>();
            match resolve_asset(lib, i["asset"].as_str().unwrap_or("")) {
                Ok(usd) => {
                    world.resource_mut::<PrototypeCache>().request(&usd);
                    let w = Wait::AssetInfo { usd, thumb: i["include_thumbnail"].as_bool().unwrap_or(false), reply, started: Instant::now() };
                    return defer(world, w);
                }
                Err(e) => ToolOutput::err(e),
            }
        }
        "download_assets" => {
            let folders: Vec<String> = i["folders"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
            let mut lib = world.resource_mut::<Library>();
            let mut unknown = Vec::new();
            let mut wanted = Vec::new();
            for f in folders {
                // Accept display names too.
                let folder = lib.entries.iter().find(|e| e.folder.eq_ignore_ascii_case(&f) || e.name.eq_ignore_ascii_case(&f)).map(|e| e.folder.clone());
                match folder {
                    Some(fo) => {
                        lib.start_download(&fo, false);
                        wanted.push(fo);
                    }
                    None => unknown.push(f),
                }
            }
            if !unknown.is_empty() {
                ToolOutput::err(format!("unknown folders: {}", unknown.join(", ")))
            } else {
                return defer(world, Wait::Download { folders: wanted, reply, started: Instant::now() });
            }
        }
        "place_objects" => {
            let mut items = Vec::new();
            let mut errors = Vec::new();
            {
                let lib = world.resource::<Library>();
                for (n, o) in i["objects"].as_array().into_iter().flatten().enumerate() {
                    let asset = o["asset"].as_str().unwrap_or("");
                    match resolve_asset(lib, asset) {
                        Ok(usd) => {
                            let yaw = f(o, "yaw_deg").unwrap_or(0.0).to_radians();
                            let s = f(o, "scale").unwrap_or(1.0).clamp(0.01, 100.0);
                            let t = Transform::from_xyz(f(o, "x").unwrap_or(0.0), f(o, "y").unwrap_or(0.0), f(o, "z").unwrap_or(0.0))
                                .with_rotation(Quat::from_rotation_y(yaw))
                                .with_scale(Vec3::splat(s));
                            let c = &o["connect_to"];
                            let connect = c["object"].as_str().map(|obj| ConnectTo {
                                target: crate::plant::connect::PortRef::new(obj, c["port"].as_str().unwrap_or("")),
                                my_port: c["my_port"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
                                aim: vec3(&c["aim"]),
                            });
                            items.push(PlaceItem {
                                usd,
                                transform: t,
                                name: o["name"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
                                on_top_of: o["on_top_of"].as_str().map(str::to_string),
                                photo: None,
                                connect,
                                ..Default::default()
                            });
                        }
                        Err(e) => errors.push(format!("object {n}: {e}")),
                    }
                }
            }
            if !errors.is_empty() {
                ToolOutput::err(format!("nothing placed:\n{}", errors.join("\n")))
            } else if items.is_empty() {
                ToolOutput::err("no objects given")
            } else {
                let mut cache = world.resource_mut::<PrototypeCache>();
                for it in &items {
                    cache.request(&it.usd);
                }
                return defer(world, Wait::Place { items, reply, started: Instant::now(), extra: Value::Null });
            }
        }
        "get_scene" => {
            let mut q = world.query::<(&PlacedObject, &Transform)>();
            let mut list: Vec<Value> = q.iter(world).map(|(o, t)| object_json(o, t)).collect();
            list.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            ToolOutput::ok(json!({ "count": list.len(), "objects": list }))
        }
        "update_object" => {
            let name = i["name"].as_str().unwrap_or("");
            match find_object(world, name) {
                None => ToolOutput::err(format!("no object named '{name}'")),
                Some(e) => {
                    let mut em = world.entity_mut(e);
                    if let Some(mut t) = em.get_mut::<Transform>() {
                        if let Some(x) = f(i, "x") {
                            t.translation.x = x;
                        }
                        if let Some(y) = f(i, "y") {
                            t.translation.y = y;
                        }
                        if let Some(z) = f(i, "z") {
                            t.translation.z = z;
                        }
                        if let Some(y) = f(i, "yaw_deg") {
                            t.rotation = Quat::from_rotation_y(y.to_radians());
                        }
                        if let Some(s) = f(i, "scale") {
                            t.scale = Vec3::splat(s.clamp(0.01, 100.0));
                        }
                    }
                    if let Some(n) = i["new_name"].as_str().filter(|s| !s.is_empty()) {
                        if let Some(mut o) = em.get_mut::<PlacedObject>() {
                            o.name = n.to_string();
                        }
                        em.insert(Name::new(n.to_string()));
                    }
                    let (o, t) = (em.get::<PlacedObject>().unwrap(), em.get::<Transform>().unwrap());
                    ToolOutput::ok(object_json(o, t))
                }
            }
        }
        "delete_objects" => {
            let mut done = Vec::new();
            let mut missing = Vec::new();
            for n in i["names"].as_array().into_iter().flatten().filter_map(|v| v.as_str()) {
                match find_object(world, n) {
                    Some(e) => {
                        world.resource_mut::<Actions>().0.push(Action::Delete(e));
                        done.push(n.to_string());
                    }
                    None => missing.push(n.to_string()),
                }
            }
            ToolOutput::ok(json!({ "deleted": done, "not_found": missing }))
        }
        "clear_scene" => {
            world.resource_mut::<Actions>().0.push(Action::ClearScene);
            ToolOutput::text("scene cleared")
        }
        "verify_scene" => {
            let v = super::verify::verify_scene(world);
            let passed = v["passed"].as_bool().unwrap_or(false);
            ToolOutput { content: vec![Block::Text(pretty(&v))], is_error: !passed }
        }
        "update_plan" => {
            let steps: Vec<(String, String)> = i["steps"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| Some((s["step"].as_str()?.to_string(), s["status"].as_str().unwrap_or("pending").to_string())))
                .collect();
            let n = steps.len();
            world.resource_mut::<Agent>().plan = steps;
            ToolOutput::text(format!("plan updated ({n} steps)"))
        }
        "frame_all" => {
            world.resource_mut::<Actions>().0.push(Action::FrameAll);
            ToolOutput::text("camera framed on all objects")
        }
        "set_camera" => match (vec3(&i["eye"]), vec3(&i["target"])) {
            (Some(eye), Some(target)) if eye.distance(target) > 1e-3 => {
                let hfov = f(i, "hfov_deg");
                set_camera(world, eye, target, hfov);
                ToolOutput::text("camera set")
            }
            _ => ToolOutput::err("eye and target must be two different [x, y, z] points"),
        },
        "screenshot" => return defer(world, Wait::Screenshot { slot: None, settled: 0, reply, started: Instant::now() }),
        "ask_user" => {
            let q = Question {
                text: i["question"].as_str().unwrap_or("?").to_string(),
                options: i["options"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect(),
                candidates: {
                    let lib = world.resource::<Library>();
                    i["candidate_assets"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str())
                        .filter_map(|s| {
                            let norm = s.replace('\\', "/").to_ascii_lowercase();
                            lib.entries.iter().position(|e| {
                                e.name.eq_ignore_ascii_case(s)
                                    || e.folder.eq_ignore_ascii_case(s)
                                    || e.usd.as_ref().map(|u| u.to_string_lossy().replace('\\', "/").to_ascii_lowercase() == norm).unwrap_or(false)
                            })
                        })
                        .collect()
                },
                allow_text: i["allow_text"].as_bool().unwrap_or(true),
                input: String::new(),
                answer: None,
                reply: Some(reply),
            };
            let mut agent = world.resource_mut::<Agent>();
            agent.questions.push(q);
            let idx = agent.questions.len() - 1;
            agent.transcript.push(ChatItem::Question(idx));
            agent.open = true;
            return None;
        }
        "perceive_photo" => {
            let id = i["image_id"].as_str().unwrap_or("").to_string();
            let Some((data, _)) = images::get(&id) else {
                return Some(ToolOutput::err(format!("unknown image_id '{id}'")));
            };
            let default_hfov = world.resource::<Agent>().config.photo_hfov_deg;
            let input = i.clone();
            // Model inference takes seconds: run off the main thread.
            std::thread::spawn(move || {
                let _ = reply.send(perceive_photo(&id, &data, default_hfov, &input));
            });
            return None;
        }
        "place_from_photo" => {
            let image = i["image_id"].as_str().unwrap_or("").to_string();
            {
                let mut scenes = super::vision::perceive::scenes().lock().unwrap();
                let Some(sc) = scenes.get_mut(&image) else {
                    return Some(ToolOutput::err(format!("no perceive_photo result for '{image}' - call perceive_photo first")));
                };
                for s in i["skip"].as_array().into_iter().flatten().filter_map(|v| v.as_u64()) {
                    sc.placed.insert(format!("#skip{s}"), s as usize);
                }
            }
            let mut items = Vec::new();
            let mut errors = Vec::new();
            {
                let lib = world.resource::<Library>();
                for it in i["items"].as_array().into_iter().flatten() {
                    let det = it["id"].as_u64().unwrap_or(u64::MAX) as usize;
                    match resolve_asset(lib, it["asset"].as_str().unwrap_or("")) {
                        Ok(usd) => items.push(PlaceItem {
                            usd,
                            transform: Transform::IDENTITY,
                            name: it["name"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
                            on_top_of: None,
                            photo: Some((image.clone(), det, f(it, "yaw_deg").map(|d| d.to_radians()))),
                            ..Default::default()
                        }),
                        Err(e) => errors.push(format!("detection {det}: {e}")),
                    }
                }
            }
            if !errors.is_empty() {
                ToolOutput::err(format!("nothing placed:\n{}", errors.join("\n")))
            } else if items.is_empty() {
                ToolOutput::text("nothing to place (all skipped)")
            } else {
                let mut cache = world.resource_mut::<PrototypeCache>();
                for it in &items {
                    cache.request(&it.usd);
                }
                return defer(world, Wait::Place { items, reply, started: Instant::now(), extra: Value::Null });
            }
        }
        "add_structure" => {
            use super::structures::{self, Kind, Piece};
            let pieces: Vec<Piece> = if let Some(img) = i["from_photo"].as_str() {
                match super::vision::perceive::scenes().lock().unwrap().get(img) {
                    Some(sc) => structures::shell_from_photo(sc),
                    None => return Some(ToolOutput::err(format!("no perceive_photo result for '{img}'"))),
                }
            } else if i["kind"].as_str().and_then(Kind::parse) == Some(Kind::Ceiling) && vec3(&i["size_m"]).is_none() {
                // Whole-scene ceiling: cover every placed object with a margin.
                let (mut lo, mut hi, mut top) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN), 0.0f32);
                {
                    let mut q = world.query::<(&PlacedObject, &Transform)>();
                    for (o, t) in q.iter(world) {
                        if matches!(Kind::of_asset(&o.asset), Some(Kind::Ceiling | Kind::Beam)) {
                            continue;
                        }
                        let (mn, mx) = world_bounds(o, t);
                        lo = lo.min(Vec2::new(mn.x, mn.z));
                        hi = hi.max(Vec2::new(mx.x, mx.z));
                        top = top.max(mx.y);
                    }
                }
                if lo.x > hi.x {
                    return Some(ToolOutput::err("the scene is empty: give size_m and x/z for the ceiling"));
                }
                let height = f(i, "height_m").unwrap_or((top + 1.0).max(3.0));
                if height < top {
                    return Some(ToolOutput::err(format!("height_m {height:.2} is below the tallest object ({top:.2} m)")));
                }
                let beams = i["with_beams"].as_bool().unwrap_or(false).then(|| f(i, "beam_spacing_m").unwrap_or(6.0));
                structures::ceiling_pieces(lo - Vec2::splat(2.0), hi + Vec2::splat(2.0), height, beams)
            } else {
                let Some(kind) = i["kind"].as_str().and_then(Kind::parse) else {
                    return Some(ToolOutput::err("give kind (floor, wall, column, block, ceiling, beam) or from_photo"));
                };
                let Some(s) = vec3(&i["size_m"]) else {
                    return Some(ToolOutput::err("size_m must be [length, height, depth]"));
                };
                vec![Piece {
                    kind,
                    size: s,
                    center: Vec2::new(f(i, "x").unwrap_or(0.0), f(i, "z").unwrap_or(0.0)),
                    base_y: f(i, "y").or(f(i, "height_m")).unwrap_or(match kind {
                        Kind::Floor => -s.y,
                        Kind::Ceiling => 4.0,
                        _ => 0.0,
                    }),
                    yaw: f(i, "yaw_deg").unwrap_or(0.0).to_radians(),
                }]
            };
            if pieces.is_empty() {
                return Some(ToolOutput::text("the photo shows no floor, walls or columns to build"));
            }
            let mut items = Vec::new();
            for p in &pieces {
                match structures::usd_for(p.kind, p.size) {
                    Ok(usd) => items.push(PlaceItem {
                        usd,
                        transform: Transform::from_xyz(p.center.x, p.base_y, p.center.y).with_rotation(Quat::from_rotation_y(p.yaw)),
                        name: if pieces.len() == 1 { i["name"].as_str().filter(|s| !s.is_empty()).map(str::to_string) } else { None },
                        on_top_of: None,
                        photo: None,
                        ..Default::default()
                    }),
                    Err(e) => return Some(ToolOutput::err(format!("could not write structure: {e}"))),
                }
            }
            let mut cache = world.resource_mut::<PrototypeCache>();
            for it in &items {
                cache.request(&it.usd);
            }
            return defer(world, Wait::Place { items, reply, started: Instant::now(), extra: Value::Null });
        }
        "optimize_layout" => {
            let image = i["image_id"].as_str().unwrap_or("").to_string();
            match super::verify::optimize_photo_layout(world, &image) {
                Ok(v) => ToolOutput::ok(v),
                Err(e) => ToolOutput::err(e),
            }
        }
        "save_layout" | "export_usda" => match i["path"].as_str().filter(|s| !s.is_empty()) {
            Some(p) => {
                let p = PathBuf::from(p);
                let action = if job.name == "save_layout" { Action::SaveLayout(p.clone()) } else { Action::ExportUsda(p.clone()) };
                world.resource_mut::<Actions>().0.push(action);
                ToolOutput::text(format!("writing {}", p.display()))
            }
            None => ToolOutput::err("path is required"),
        },
        "load_reference_image" => match images::load_file(Path::new(i["path"].as_str().unwrap_or(""))) {
            Ok((id, data, mime)) => ToolOutput {
                content: vec![Block::Text(format!("image_id: {id}")), Block::Image { data, mime }],
                is_error: false,
            },
            Err(e) => ToolOutput::err(e),
        },
        other if other.starts_with("plant_") => {
            let s = crate::plant::tools::start(world, other, i);
            return run_started(world, s, reply);
        }
        other if other.starts_with("rtls_") => match crate::rtls::tools::start(world, other, i) {
            Some(out) => out,
            None => ToolOutput::err(format!("unknown tool '{other}'")),
        },
        other => ToolOutput::err(format!("unknown tool '{other}'")),
    };
    Some(out)
}

fn defer(world: &mut World, w: Wait) -> Option<ToolOutput> {
    world.resource_mut::<PendingTools>().0.push(w);
    None
}

/// Checks a parked job; returns it back if it still has to wait.
fn poll(world: &mut World, w: Wait) -> Option<Wait> {
    match w {
        Wait::AssetInfo { usd, thumb, reply, started } => {
            let cache = world.resource::<PrototypeCache>();
            let out = match cache.states.get(&usd) {
                Some(ProtoState::Ready(p)) => {
                    let size = p.aabb_max - p.aabb_min;
                    let mut out = ToolOutput::ok(json!({
                        "asset": usd.to_string_lossy().replace('\\', "/"),
                        "size_m": [r2(size.x), r2(size.y), r2(size.z)],
                        "triangles": p.triangles,
                        "parts": p.parts.len(),
                        "joints": p.joints.len(),
                    }));
                    if thumb {
                        let folder = usd.parent().and_then(|d| d.parent()).and_then(|d| d.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        if let Some(png) = thumbnail_png(Some(&usd), &folder) {
                            out.content.push(png_block(png));
                        }
                    }
                    out
                }
                Some(ProtoState::Failed(e)) => ToolOutput::err(format!("failed to load: {e}")),
                _ if started.elapsed() > LOAD_TIMEOUT => ToolOutput::err("timed out loading the asset"),
                _ => return Some(Wait::AssetInfo { usd, thumb, reply, started }),
            };
            let _ = reply.send(out);
            None
        }
        Wait::Download { folders, reply, started } => {
            let lib = world.resource::<Library>();
            if folders.iter().any(|f| lib.download_state(f).is_some()) && started.elapsed() < DOWNLOAD_TIMEOUT {
                return Some(Wait::Download { folders, reply, started });
            }
            let mut ok = Vec::new();
            let mut failed = Vec::new();
            for f in &folders {
                match lib.entries.iter().find(|e| &e.folder == f).and_then(|e| e.usd.clone()) {
                    Some(u) => ok.push(json!({ "folder": f, "asset": u.to_string_lossy().replace('\\', "/") })),
                    None => failed.push(f.clone()),
                }
            }
            let out = if failed.is_empty() {
                ToolOutput::ok(json!({ "downloaded": ok }))
            } else {
                ToolOutput { content: vec![Block::Text(pretty(&json!({ "downloaded": ok, "failed": failed })))], is_error: ok.is_empty() }
            };
            let _ = reply.send(out);
            None
        }
        Wait::Screenshot { slot: None, settled, reply, started } => {
            // Keep the UI overlays (properties card, toolbar…) out of the capture.
            if let Some(mut h) = world.get_resource_mut::<crate::ui::HideOverlays>() {
                h.0 = h.0.max(4);
            }
            // New assets need a few frames for meshes and textures to reach the GPU.
            let fresh = world.resource::<PendingTools>().1.is_some_and(|t| t.elapsed() < Duration::from_secs(3));
            let busy = fresh || world.resource::<PrototypeCache>().loading_count() > 0;
            let settled = if busy { 0 } else { settled + 1 };
            if settled < 10 && started.elapsed() < Duration::from_secs(60) {
                return Some(Wait::Screenshot { slot: None, settled, reply, started });
            }
            let slot: Arc<Mutex<Option<Image>>> = Arc::default();
            let s = slot.clone();
            world.spawn(Screenshot::primary_window()).observe(move |ev: On<ScreenshotCaptured>| {
                *s.lock().unwrap() = Some(ev.image.clone());
            });
            Some(Wait::Screenshot { slot: Some(slot), settled, reply, started: Instant::now() })
        }
        Wait::Screenshot { slot: Some(slot), settled, reply, started } => {
            let img = slot.lock().unwrap().take();
            match img {
                Some(img) => {
                    let _ = reply.send(screenshot_output(world, img));
                    None
                }
                None if started.elapsed() > Duration::from_secs(10) => {
                    let _ = reply.send(ToolOutput::err("screenshot timed out"));
                    None
                }
                None => Some(Wait::Screenshot { slot: Some(slot), settled, reply, started }),
            }
        }
        Wait::Place { items, reply, started, extra } => {
            let cache = world.resource::<PrototypeCache>();
            let pending = items.iter().any(|it| !matches!(cache.states.get(&it.usd), Some(ProtoState::Ready(_)) | Some(ProtoState::Failed(_))));
            if pending && started.elapsed() < LOAD_TIMEOUT {
                return Some(Wait::Place { items, reply, started, extra });
            }
            let _ = reply.send(place(world, items, extra));
            None
        }
        Wait::After { usds, then, reply, started } => {
            let cache = world.resource::<PrototypeCache>();
            let pending = usds.iter().any(|u| !matches!(cache.states.get(u), Some(ProtoState::Ready(_)) | Some(ProtoState::Failed(_))));
            if pending && started.elapsed() < LOAD_TIMEOUT {
                return Some(Wait::After { usds, then, reply, started });
            }
            let next = then(world);
            if let Some(out) = run_started(world, next, reply.clone()) {
                let _ = reply.send(out);
            }
            None
        }
    }
}

/// World pose of every placed object, for connecting new ones to them.
type Poses = std::collections::HashMap<String, (Transform, PathBuf, Arc<crate::prototype::Prototype>)>;

/// Transform that mates `my` object (prototype `p`, asset `usd`) with the target port.
fn connect_pose(c: &ConnectTo, usd: &Path, p: &crate::prototype::Prototype, t: &Transform, poses: &Poses, used: &std::collections::HashSet<crate::plant::connect::PortRef>, name: &str) -> Result<(Transform, crate::plant::connect::Link), String> {
    use crate::plant::{connect, ports};
    let (tt, tasset, tproto) = poses.get(&c.target.object).ok_or_else(|| format!("connect_to: no object named '{}'", c.target.object))?;
    let tinfo = ports::info(tasset, tproto);
    let ids = |i: &ports::Info| i.ports.iter().map(|p| format!("{} ({})", p.id, p.kind.name())).collect::<Vec<_>>().join(", ");
    let tport = tinfo.port(&c.target.port).ok_or_else(|| format!("{} has no port '{}'; its ports: {}", c.target.object, c.target.port, ids(&tinfo)))?;
    let minfo = ports::info(usd, p);
    let mine = match &c.my_port {
        Some(id) => minfo.port(id).ok_or_else(|| format!("this asset has no port '{id}'; its ports: {}", ids(&minfo)))?,
        None => minfo
            .ports
            .iter()
            .find(|q| ports::PortKind::mates(tport.kind, q.kind).is_some() && !used.contains(&connect::PortRef::new(name, &q.id)))
            .ok_or_else(|| format!("this asset has no port that mates with a {} port; its ports: {}", tport.kind.name(), ids(&minfo)))?,
    };
    let kind = connect::link_kind(tport, mine)?;
    let free = minfo.spec.is_some() && matches!(minfo.role, ports::Role::Fitting | ports::Role::Valve | ports::Role::Rotor);
    let turn = if free { connect::Turn::Free { aim: c.aim } } else { connect::Turn::Yaw };
    let other = minfo.ports.iter().find(|q| q.id != mine.id && q.kind == mine.kind);
    let (tp, td) = tport.world(tt);
    let pose = if kind == connect::LinkKind::Belt {
        *t
    } else {
        connect::snap(tp, td, kind, mine, other, t, turn)?
    };
    Ok((pose, connect::Link { a: c.target.clone(), b: connect::PortRef::new(name, &mine.id), kind }))
}

fn place(world: &mut World, items: Vec<PlaceItem>, extra: Value) -> ToolOutput {
    world.resource_mut::<PendingTools>().1 = Some(Instant::now());
    // Stack targets are resolved before spawning anything.
    let mut tops = std::collections::HashMap::new();
    let mut poses: Poses = Poses::new();
    {
        let mut q = world.query::<(&PlacedObject, &Transform)>();
        for (o, t) in q.iter(world) {
            tops.insert(o.name.clone(), world_bounds(o, t).1.y);
            poses.insert(o.name.clone(), (*t, o.asset.clone(), o.proto.clone()));
        }
    }
    let mut used: std::collections::HashSet<crate::plant::connect::PortRef> =
        world.resource::<crate::plant::Links>().0.iter().flat_map(|l| [l.a.clone(), l.b.clone()]).collect();
    let mut new_links: Vec<crate::plant::connect::Link> = Vec::new();
    let mut state: SystemState<(Commands, ResMut<NameCounter>, Res<PrototypeCache>)> = SystemState::new(world);
    let mut placed = Vec::new();
    let mut errors = Vec::new();
    {
        let Ok((mut commands, mut counter, cache)) = state.get_mut(world) else { return ToolOutput::err("scene unavailable") };
        for it in items {
            let mut t = it.transform;
            if let Some(base) = &it.on_top_of {
                match tops.get(base) {
                    Some(y) => t.translation.y = *y,
                    None => errors.push(format!("on_top_of: no object named '{base}', placed on the floor")),
                }
            }
            match cache.states.get(&it.usd) {
                Some(ProtoState::Ready(p)) => {
                    let name = it.name.clone().unwrap_or_else(|| unique(&mut counter, &it.usd));
                    let p = p.clone();
                    // Photo placement: fit the real asset size to the measurement.
                    if let Some((image, id, yaw)) = &it.photo {
                        let mut scenes = super::vision::perceive::scenes().lock().unwrap();
                        if let Some(sc) = scenes.get_mut(image)
                            && let Some(obj) = sc.objects.get(*id).cloned()
                        {
                            let f = super::placer::fit(sc, &obj, p.aabb_max - p.aabb_min, *yaw);
                            t = Transform::from_translation(f.translation)
                                .with_rotation(Quat::from_rotation_y(f.yaw))
                                .with_scale(Vec3::splat(f.scale));
                            sc.placed.insert(name.clone(), *id);
                        } else {
                            errors.push(format!("{name}: no detection #{id} in {image}; placed at the given position"));
                        }
                    }
                    let mut connected = None;
                    if let Some(c) = &it.connect {
                        match connect_pose(c, &it.usd, &p, &t, &poses, &used, &name) {
                            Ok((pose, link)) => {
                                t = pose;
                                connected = Some(format!("{} ↔ {} ({})", link.a.label(), link.b.label(), link.kind.name()));
                                used.insert(link.a.clone());
                                used.insert(link.b.clone());
                                new_links.push(link);
                            }
                            Err(e) => {
                                errors.push(format!("{name}: not placed: {e}"));
                                continue;
                            }
                        }
                    }
                    spawn_object(&mut commands, &mut counter, &p, &it.usd, t, Some(name.clone()), None);
                    poses.insert(name.clone(), (t, it.usd.clone(), p.clone()));
                    for l in &it.links {
                        used.insert(l.a.clone());
                        used.insert(l.b.clone());
                    }
                    new_links.extend(it.links.iter().cloned());
                    let size = (p.aabb_max - p.aabb_min) * t.scale;
                    tops.insert(name.clone(), t.translation.y + size.y);
                    let mut entry = json!({
                        "name": name,
                        "position": [r2(t.translation.x), r2(t.translation.y), r2(t.translation.z)],
                        "size_m": [r2(size.x), r2(size.y), r2(size.z)],
                    });
                    if let Some(c) = connected {
                        let (yaw, _, _) = t.rotation.to_euler(EulerRot::YXZ);
                        entry["connected"] = json!(c);
                        entry["yaw_deg"] = json!(r2(yaw.to_degrees()));
                    }
                    if let Some((_, id, _)) = &it.photo {
                        let (yaw, _, _) = t.rotation.to_euler(EulerRot::YXZ);
                        entry["detection"] = json!(id);
                        entry["yaw_deg"] = json!(r2(yaw.to_degrees()));
                        if t.scale.x != 1.0 {
                            entry["scale"] = json!(r2(t.scale.x));
                        }
                    }
                    placed.push(entry);
                }
                Some(ProtoState::Failed(e)) => errors.push(format!("{}: failed to load: {e}", it.usd.display())),
                _ => errors.push(format!("{}: timed out loading", it.usd.display())),
            }
        }
    }
    state.apply(world);
    let n_links = new_links.len();
    {
        let mut links = world.resource_mut::<crate::plant::Links>();
        for l in new_links {
            links.add(l);
        }
    }
    // Long assemblies: list names compactly.
    let mut v = if placed.len() > 12 {
        json!({ "placed_count": placed.len(), "placed": placed.iter().map(|e| e["name"].clone()).collect::<Vec<_>>() })
    } else {
        json!({ "placed": placed })
    };
    if n_links > 0 {
        v["links_added"] = json!(n_links);
    }
    if let Value::Object(m) = extra {
        for (k, x) in m {
            v[k] = x;
        }
    }
    if !errors.is_empty() {
        v["errors"] = json!(errors);
    }
    let failed = v["placed"].as_array().is_none_or(|a| a.is_empty());
    ToolOutput { content: vec![Block::Text(pretty(&v))], is_error: failed }
}

/// Same naming scheme as the scene's own placement (`folder_N`).
fn unique(counter: &mut NameCounter, asset: &Path) -> String {
    let base = asset
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .or_else(|| asset.file_stem())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "asset".into());
    let n = counter.0.entry(base.clone()).or_insert(0);
    *n += 1;
    format!("{base}_{n}")
}

fn set_camera(world: &mut World, eye: Vec3, target: Vec3, hfov: Option<f32>) {
    let mut q = world.query::<(&mut OrbitCamera, &mut Projection, &Camera)>();
    for (mut orbit, mut proj, cam) in q.iter_mut(world) {
        let d = eye - target;
        let r = d.length();
        let n = d / r;
        orbit.focus = target;
        orbit.radius = r;
        orbit.pitch = n.y.clamp(-1.0, 1.0).asin().clamp(-1.5, 1.5);
        orbit.yaw = n.x.atan2(n.z);
        if let Some(h) = hfov
            && let Projection::Perspective(p) = &mut *proj
        {
            let size = cam.logical_viewport_size().unwrap_or(Vec2::new(16.0, 9.0));
            let aspect = (size.x / size.y.max(1.0)).max(0.1);
            p.fov = 2.0 * ((h.clamp(10.0, 150.0).to_radians() * 0.5).tan() / aspect).atan();
        }
    }
}

/// Crops the window capture to the 3D viewport and encodes it.
fn screenshot_output(world: &mut World, img: Image) -> ToolOutput {
    let viewport = {
        let mut q = world.query::<(&Camera, &OrbitCamera)>();
        q.iter(world).next().and_then(|(c, _)| c.viewport.as_ref().map(|v| (v.physical_position, v.physical_size)))
    };
    let Ok(dynimg) = img.try_into_dynamic() else { return ToolOutput::err("could not convert the screenshot") };
    let mut rgba = dynimg.to_rgba8();
    if let Some((pos, size)) = viewport {
        let (w, h) = (rgba.width(), rgba.height());
        let (x, y) = (pos.x.min(w.saturating_sub(1)), pos.y.min(h.saturating_sub(1)));
        let (cw, ch) = (size.x.min(w - x), size.y.min(h - y));
        if cw > 8 && ch > 8 {
            rgba = image::imageops::crop_imm(&rgba, x, y, cw, ch).to_image();
        }
    }
    let mut d = image::DynamicImage::ImageRgba8(rgba);
    if d.width() > 1280 {
        d = d.resize(1280, 1280, image::imageops::FilterType::Triangle);
    }
    let d = image::DynamicImage::ImageRgb8(d.to_rgb8());
    match encode_png(&d) {
        Some(png) => {
            let data = Arc::new(png);
            let id = images::put(data.clone(), "image/png");
            world.resource_mut::<Agent>().transcript.push(ChatItem::Image(id.clone()));
            ToolOutput { content: vec![Block::Text(format!("viewport screenshot ({id})")), Block::Image { data, mime: "image/png".into() }], is_error: false }
        }
        None => ToolOutput::err("could not encode the screenshot"),
    }
}

fn perceive_photo(image_id: &str, data: &[u8], default_hfov: f32, i: &Value) -> ToolOutput {
    use super::vision::perceive;
    let labels: Vec<String> = i["labels"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
    let extra: Vec<(String, [f32; 4])> = i["extra_boxes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|b| {
            let v = |k| f(b, k).unwrap_or(0.0).clamp(0.0, 1.0);
            (b["label"].as_str().unwrap_or("object").to_string(), [v("x0"), v("y0"), v("x1"), v("y1")])
        })
        .collect();
    let ks = &i["known_size"];
    let known = ks["id"].as_u64().and_then(|id| match (f(ks, "width_m"), f(ks, "height_m")) {
        (Some(w), _) => Some((id as usize, w, false)),
        (None, Some(h)) => Some((id as usize, h, true)),
        _ => None,
    });
    let req = perceive::Request {
        image_id,
        bytes: data,
        labels: &labels,
        extra_boxes: &extra,
        hfov_deg: f(i, "hfov_deg"),
        default_hfov,
        known,
        threshold: f(i, "threshold").unwrap_or(0.2).clamp(0.05, 0.9),
    };
    let scene = match perceive::perceive(&req) {
        Ok(s) => s,
        Err(e) => {
            return ToolOutput::err(format!(
                "{e:#}\nVision is unavailable for this photo: estimate positions from perspective and the user's dimensions, and say so."
            ));
        }
    };
    let mut v = scene.to_json();
    v["detections"] = json!(scene.objects.len());
    let mut out = ToolOutput { content: vec![Block::Text(pretty(&v))], is_error: false };
    if let Ok(photo) = image::load_from_memory(data) {
        let overlay = scene.overlay(&photo.to_rgb8(), &[]);
        let mut d = image::DynamicImage::ImageRgb8(overlay);
        if d.width() > 1280 {
            d = d.resize(1280, 1280, image::imageops::FilterType::Triangle);
        }
        if let Some(png) = encode_png(&d) {
            out.content.push(Block::Text("Detections (green, numbered by id):".into()));
            out.content.push(png_block(png));
        }
    }
    out
}
