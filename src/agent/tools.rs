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
use super::gate::{self, CallCtx, Origin, Policy, Verdict};
use super::llm::{Block, ToolDef};
use super::{Agent, ChatItem, Question, assetgen, images, photocheck, toolmaker};
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
    /// Who asked, and what a macro's steps inherit.
    pub ctx: CallCtx,
    /// Only to ask the gate: the caller runs the tool itself once it's allowed.
    pub check_only: bool,
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
    let origin = if from_mcp { Origin::Mcp } else { Origin::Assistant };
    send_and_wait(tx, name, input, CallCtx { origin, ..Default::default() }, cancel)
}

/// As [`call`], saying who is asking and what their calls should carry.
pub fn call_ctx(tx: &Sender<ToolJob>, name: &str, input: Value, ctx: CallCtx) -> ToolOutput {
    let cancel = ctx.cancel.clone();
    send_and_wait(tx, name, input, ctx, cancel.as_deref())
}

fn send_and_wait(tx: &Sender<ToolJob>, name: &str, input: Value, ctx: CallCtx, cancel: Option<&AtomicBool>) -> ToolOutput {
    // Waiting has to happen on this thread, not in the app's update loop.
    #[cfg(feature = "antenna-studio")]
    if name == crate::antenna::assist::WAIT {
        return crate::antenna::assist::wait(tx, &input, cancel);
    }
    #[cfg(feature = "antenna-studio")]
    if name == crate::antenna::tagjob::TRY {
        return crate::antenna::tagjob::try_structure(tx, &input, ctx);
    }
    let (rtx, rrx) = unbounded();
    send_job(tx, ToolJob { name: name.into(), input, reply: rtx, ctx, check_only: false }, rrx, cancel)
}

/// Asks the gate about a tool that runs on the caller's thread, and waits for the verdict. An
/// error says why not; anything else means go ahead.
pub fn gate_only(tx: &Sender<ToolJob>, name: &str, input: Value, ctx: CallCtx) -> ToolOutput {
    let cancel = ctx.cancel.clone();
    let (rtx, rrx) = unbounded();
    send_job(tx, ToolJob { name: name.into(), input, reply: rtx, ctx, check_only: true }, rrx, cancel.as_deref())
}

fn send_job(tx: &Sender<ToolJob>, job: ToolJob, rrx: Receiver<ToolOutput>, cancel: Option<&AtomicBool>) -> ToolOutput {
    if tx.send(job).is_err() {
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
    let material = json!({
        "description": "A preset name, or an object. Fields on top of a preset: color, emissive, roughness, metallic, opacity, tile_m. texture is an image file; photo {image_id, box: [x0,y0,x1,y1]} (or {image_id, id} for a detection) textures with that crop of the photo, or samples its color with as: \"color\".",
        "oneOf": [
            { "type": "string", "enum": assetgen::PRESETS },
            {
                "type": "object",
                "properties": {
                    "preset": { "type": "string", "enum": assetgen::PRESETS },
                    "color": { "description": "#rrggbb or [r, g, b] 0-1, sRGB" },
                    "emissive": { "description": "#rrggbb or [r, g, b] 0-1, sRGB" },
                    "roughness": { "type": "number" }, "metallic": { "type": "number" }, "opacity": { "type": "number" },
                    "texture": { "type": "string" },
                    "tile_m": { "type": "number", "description": "real size of one texture repeat" },
                    "photo": { "type": "object" }
                }
            }
        ]
    });
    let layout_camera = json!({
        "type": "object",
        "description": "your reading of the camera from the photo. The horizon is at eye level, where parallel floor and ceiling lines converge; a tilted photo has different left and right values.",
        "properties": {
            "height_m": { "type": "number", "description": "camera height above the floor" },
            "horizon_v": { "type": "number", "description": "normalized row of the horizon, 0 at the top, for a level photo" },
            "horizon_left_v": { "type": "number", "description": "row where the horizon meets the left edge" },
            "horizon_right_v": { "type": "number", "description": "row where the horizon meets the right edge" }
        },
        "required": ["height_m"]
    });
    let layout_objects = json!({
        "type": "array",
        "description": "your estimates per detection. An object whose box reaches the floor needs none to be placed, but sizes check your camera. Hanging things and things with a hidden base need one.",
        "items": {
            "type": "object",
            "properties": {
                "id": { "type": "integer" },
                "height_m": { "type": "number", "description": "real height, from what it is" },
                "width_m": { "type": "number", "description": "real width across the view" },
                "base_y": { "type": "number", "description": "height of its underside: on a shelf, or hanging" },
                "at_xz": { "type": "array", "items": { "type": "number" }, "description": "floor position [x, z], when you know better than perspective" },
                "yaw_deg": { "type": "number", "description": "which way it faces" }
            },
            "required": ["id"]
        }
    });
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
            "Move, rotate, rescale or rename a placed object. Omitted fields stay unchanged. An object the user locked (\"locked\": true in get_scene) can only be renamed.",
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
            "Delete placed objects by name. Objects the user locked are kept and listed in locked_not_deleted.",
            json!({ "type": "object", "properties": { "names": { "type": "array", "items": { "type": "string" } } }, "required": ["names"] }),
        ),
        def("clear_scene", "Remove every placed object.", json!({ "type": "object", "properties": {} })),
        def(
            "load_template",
            &format!(
                "Replace the scene with a ready-made starting scene (room, objects, RTLS devices, zones, moving tags) in one call. \
                 When the request resembles a template, load it first and adapt it (update_object / delete_objects / place_objects / rtls_*) \
                 instead of building from scratch. Missing assets download automatically. Templates:\n{}",
                crate::samples::TEMPLATES.iter().map(|(id, s)| format!("- {id}: {}", s.blurb)).collect::<Vec<_>>().join("\n")
            ),
            json!({
                "type": "object",
                "properties": { "name": { "type": "string", "enum": crate::samples::TEMPLATES.iter().map(|(id, _)| *id).collect::<Vec<_>>() } },
                "required": ["name"]
            }),
        ),
        def(
            "verify_scene",
            "Measure the scene for physical problems: objects floating above or sunk into what is below them, solid objects intersecting each other, and positioning devices or hanging fixtures in mid-air with nothing holding them. Call it after placing or moving things and fix every issue it reports; the harness also runs it automatically before you finish.",
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
            "set_photo_layout",
            "State or revise your reading of a photo, instantly and without running a model: the camera (height and horizon), field of view, building axis, and per detection a size, a base height, a position or a yaw. Everything is recomputed by perspective and returned, with the overlay. Only what you pass changes. Read each object's checks: where its size and floor contact disagree, or implied_camera_height_m differs from your camera, something you stated is wrong.",
            json!({
                "type": "object",
                "properties": {
                    "image_id": { "type": "string" },
                    "camera": layout_camera,
                    "hfov_deg": { "type": "number" },
                    "building_axis_deg": { "type": "number", "description": "which way the building runs relative to the camera: 0 means its walls and aisles are square to the view" },
                    "objects": layout_objects
                },
                "required": ["image_id"]
            }),
        ),
        def(
            "check_against_depth",
            "Verification only: reads the photo's depth with a vision model and reports where your finished layout disagrees with it. Changes nothing. Depth compresses with distance and misjudges glass and dense shelves, so a disagreement is a reason to look at the photo again, not an answer.",
            json!({ "type": "object", "properties": { "image_id": { "type": "string" } }, "required": ["image_id"] }),
        ),
        def(
            "add_photo_backdrop",
            "Put a region of the photo in the scene as a flat picture at a distance you estimate, sized so that from the photo camera it covers exactly that region of the view. For what you can't rebuild as objects: the far end of the store past the last modeled shelf, a mezzanine, a wall of signs. It looks right from the photo's viewpoint and flat from anywhere else, and it isn't solid to simulation, so say which parts of the scene are backdrops.",
            json!({
                "type": "object",
                "properties": {
                    "image_id": { "type": "string" },
                    "box": { "type": "array", "items": { "type": "number" }, "description": "normalized [x0, y0, x1, y1]" },
                    "distance_m": { "type": "number", "description": "your estimate of how far away this part of the photo is" },
                    "name": { "type": "string" }
                },
                "required": ["image_id", "box", "distance_m"]
            }),
        ),
        def(
            "compare_to_photo",
            "Verify your work: renders the scene from the photo's camera and returns the real photo beside it, with a blend and a difference map, plus measured color_match, detail_match (0-1) and the regions that differ most. Look at it and believe it over your expectations. Moves the camera to the photo's viewpoint.",
            json!({ "type": "object", "properties": { "image_id": { "type": "string" } }, "required": ["image_id"] }),
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
            "Find the objects in a reference photo (open-vocabulary detection): returns numbered boxes and an overlay image with your horizon drawn on it. This does not know where things are: positions follow from the camera you state, by perspective, so give camera and objects here or with set_photo_layout. Labels should be short concrete nouns, e.g. [\"shelf\", \"pallet\", \"forklift\", \"column\", \"hanging sign\"]. Add extra_boxes for what the detector misses.",
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
                    "camera": layout_camera.clone(),
                    "hfov_deg": { "type": "number", "description": "your estimate of the horizontal field of view; otherwise EXIF, or the configured default" },
                    "objects": layout_objects.clone(),
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
            "Add building structure that isn't in the asset library: floor slab, walls, columns, ceiling, roof beams, plain blocks or a glass door. Either give one piece (kind, size_m = [length, height, depth], x, z, yaw_deg, material), or from_photo = image_id to build the room shell you estimated: floor, walls, columns and always a ceiling, painted with the photo. Give it room and height_m, your estimates; refine it with materials; pass ceiling false only for an outdoor photo. A ceiling without size_m covers the whole scene at height_m (default: tallest object + 1 m); with_beams adds roof trusses under it. Ceilings are see-through from above and are the main mounting surface for RTLS anchors. A wall or other piece with a see-through material such as glass counts as glass in radio predictions. kind glass_door is a closed glass door in an aluminium frame, size_m = [width, height, any], default 1.8 by 2.1 m: stand it in a gap in the wall, since inside a solid wall radio still pays for the wall, close the wall above it with a wall piece at y = its height, and put door, entrance or exit in its name so RFID portals find it.",
            json!({
                "type": "object",
                "properties": {
                    "from_photo": { "type": "string" },
                    "kind": { "type": "string", "enum": ["floor", "wall", "column", "block", "ceiling", "roof", "beam", "glass_door"] },
                    "size_m": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                    "x": { "type": "number" }, "z": { "type": "number" }, "y": { "type": "number" },
                    "height_m": { "type": "number", "description": "ceiling height" },
                    "with_beams": { "type": "boolean" },
                    "beam_spacing_m": { "type": "number", "description": "default 6" },
                    "yaw_deg": { "type": "number" },
                    "name": { "type": "string" },
                    "material": material.clone(),
                    "materials": {
                        "type": "object",
                        "description": "from_photo: per-surface materials on top of the sampled colors, e.g. {\"ceiling\": {\"preset\": \"ceiling_tile\"}, \"floor\": {\"preset\": \"vinyl_tile\", \"color\": \"#c9c4b8\"}}",
                        "properties": { "floor": material.clone(), "wall": material.clone(), "column": material.clone(), "ceiling": material.clone() }
                    },
                    "ceiling": { "type": "boolean", "description": "from_photo: false when the photo is outdoors. Default true." },
                    "project_photo": { "type": "boolean", "description": "from_photo: paint the photo onto the floor, ceiling and assumed walls, so they look right from its camera. Default true. A surface given in materials keeps that material instead." },
                    "room": {
                        "type": "object",
                        "description": "from_photo, required the first time: your estimate of the whole room from the camera, including what's out of view: meters to the left and right walls and to the far wall. Sticks for later calls.",
                        "properties": { "left_m": { "type": "number" }, "right_m": { "type": "number" }, "depth_m": { "type": "number" } },
                        "required": ["left_m", "right_m", "depth_m"]
                    }
                }
            }),
        ),
        def(
            "create_asset",
            "Make a new asset when the library has nothing that looks right, then place it like any other (place_objects or place_from_photo, by name; search_assets finds it from now on). Compose it from parts, or pass from_detection to get a stand-in with the measured size and the photo crop on its front, which looks exactly like the real object from the photo camera. parts may use ${expression} and {\"$repeat\": n, \"as\": \"i\", \"item\": {...}} for rows of shelves and the like. Meters; Y up; the finished asset's origin is moved to its bottom center. Re-using a name replaces that asset.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "description": { "type": "string" },
                    "mount": { "type": "string", "enum": ["floor", "ceiling", "wall"], "description": "ceiling or wall for lights, signs and anything else that hangs, so it stays up; place it touching that surface, since verify_scene reports one left in mid-air. Default floor" },
                    "parts": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "shape": { "type": "string", "enum": ["box", "cylinder", "cone", "sphere", "panel"], "description": "panel: one quad facing +Z, for signs, screens and glass; default box" },
                                "size": { "type": "array", "items": { "type": "number" }, "description": "[x, y, z] extent in m; panel uses [width, height, any]" },
                                "at": { "type": "array", "items": { "type": "number" }, "description": "center [x, y, z]; default origin" },
                                "rotate_deg": { "type": "array", "items": { "type": "number" }, "description": "[x, y, z]" },
                                "top_scale": { "type": "number", "description": "cylinder: top end relative to the bottom (tapers)" },
                                "material": material.clone()
                            },
                            "required": ["size"]
                        }
                    },
                    "from_detection": {
                        "type": "object",
                        "properties": {
                            "image_id": { "type": "string" }, "id": { "type": "integer" },
                            "depth_m": { "type": "number", "description": "depth along the view, which a photo can't measure" },
                            "width_m": { "type": "number", "description": "your estimate, if better than the measured width" },
                            "height_m": { "type": "number", "description": "your estimate, if better than the measured height" }
                        },
                        "required": ["image_id", "id"]
                    }
                },
                "required": ["name"]
            }),
        ),
        def(
            "set_object_material",
            "Change the look of a placed object: any library asset or generated piece. Recolors all of it, replacing its base texture unless keep_texture (which tints it instead). Use the color perceive_photo measured for that object. Saved layouts keep it; USDA export to other tools shows the original look of library assets. A see-through look (the glass preset, or opacity under 0.9) also makes it glass in radio predictions; other looks leave radio alone.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "placed object" },
                    "preset": { "type": "string", "enum": assetgen::PRESETS },
                    "color": { "type": "string", "description": "#rrggbb" },
                    "roughness": { "type": "number" }, "metallic": { "type": "number" }, "opacity": { "type": "number" },
                    "keep_texture": { "type": "boolean" }
                },
                "required": ["name"]
            }),
        ),
        def(
            "define_tool",
            "Make a new tool for yourself when you would otherwise repeat a sequence of calls, or need a builder the app lacks. It is saved across sessions and offered as custom_<name> from your next turn. It runs steps in order, each an existing tool call; inputs may use ${parameter}, ${expression} (+ - * / %, min max floor ceil round abs sqrt sin cos), and {\"$repeat\": n, \"as\": \"i\", \"item\": {...}} inside arrays. Parameters without a default are required.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "description": { "type": "string", "description": "when to use it and what it does" },
                    "parameters": { "type": "object", "description": "name -> {type, description, default}" },
                    "steps": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": { "tool": { "type": "string" }, "input": { "type": "object" } },
                            "required": ["tool", "input"]
                        }
                    }
                },
                "required": ["name", "description", "steps"]
            }),
        ),
        def(
            "delete_tool",
            "Delete a tool saved with define_tool.",
            json!({ "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] }),
        ),
        def(
            "save_layout",
            "Save the project as a .sesim file: the scene with its devices, plant and studies.",
            json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
        ),
        def(
            "export_usda",
            "Export the scene as a Y-up USDA stage referencing the original assets (opens in Isaac Sim / Omniverse / usdview).",
            json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
        ),
    ];
    v.push(def(
        "get_screen",
        "What the user has on screen right now: workspace, open card or workspace, scene, selection, the tool they're using, what is running, and what they did lately. Their message already says this as it stood when they sent it; call this during a long task, when they may have moved on.",
        json!({ "type": "object", "properties": {} }),
    ));
    if mcp {
        v.push(def(
            "load_reference_image",
            "Load an image file (a photo of the scene to rebuild) and return its image_id for estimate_depth, plus the image itself.",
            json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
        ));
    }
    v.extend(crate::rtls::tools::defs());
    v.extend(crate::outcomes::tools::defs());
    v.push(bim_tool_def());
    v.extend(crate::plant::tools::defs());
    v.extend(crate::sim::tools::defs());
    v.extend(toolmaker::defs());
    #[cfg(feature = "antenna-studio")]
    v.extend(crate::antenna::tools::defs());
    v.extend(super::knowledge::tool_defs());
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
    /// `compare`: the photo to set the capture against.
    Screenshot { slot: Option<Arc<Mutex<Option<Image>>>>, settled: u32, reply: Sender<ToolOutput>, started: Instant, compare: Option<String> },
    Download { folders: Vec<String>, reply: Sender<ToolOutput>, started: Instant },
    /// The gate asked the user. `answer` is the question card's reply; (label, key) is what a
    /// "for this chat" answer would remember.
    Approval { job: ToolJob, answer: Receiver<ToolOutput>, remember: Option<(String, String)> },
}

#[derive(Resource, Default)]
pub struct PendingTools(Vec<Wait>, Option<Instant>);

pub fn run_tool_jobs(world: &mut World) {
    let jobs: Vec<ToolJob> = world.resource::<ToolBridge>().rx.try_iter().collect();
    for job in jobs {
        if job.ctx.origin == Origin::Mcp {
            // To the log, not the chat: that's the user's conversation with the assistant, and a
            // client can make hundreds of calls.
            debug!("MCP client → {} {}", job.name, short(&job.input.to_string(), 160));
        }
        run_gated(world, job);
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

/// The one way into [`start`]: every call the bridge carries is judged here first.
fn run_gated(world: &mut World, job: ToolJob) {
    let reply = job.reply.clone();
    // A call can still be queued after Stop: its caller has gone, and it mustn't act now.
    if job.ctx.cancelled() {
        let _ = reply.send(ToolOutput::err("cancelled by the user"));
        return;
    }
    let verdict = {
        let agent = world.resource::<Agent>();
        let mut roots = vec![gate::workspace_root()];
        roots.extend(agent.project_dir.clone());
        gate::decide(&job.name, &job.input, &job.ctx, &Policy { mode: agent.config.approval, approved: &agent.approved, roots: &roots, external: agent.mcp.has_tool(&job.name) })
    };
    match verdict {
        Verdict::Run => go_ahead(world, job),
        Verdict::Refuse(why) => {
            let _ = reply.send(ToolOutput::err(why));
        }
        // Parked, never waited on: this is the main thread.
        Verdict::Ask(approval) => {
            let (tx, answer) = unbounded();
            let mut options = vec![ALLOW.to_string()];
            options.extend(approval.remember.as_ref().map(|(label, _)| label.clone()));
            options.push("Deny".to_string());
            let question = Question { text: approval.text, options, candidates: Vec::new(), allow_text: false, input: String::new(), answer: None, reply: Some(tx), approval: true };
            let mut agent = world.resource_mut::<Agent>();
            agent.questions.push(question);
            let idx = agent.questions.len() - 1;
            agent.transcript.push(ChatItem::Question(idx));
            agent.open = true;
            let _ = defer(world, Wait::Approval { job, answer, remember: approval.remember });
        }
    }
}

const ALLOW: &str = "Allow";

/// The gate has agreed: runs the tool, or tells a caller that runs it itself.
fn go_ahead(world: &mut World, job: ToolJob) {
    let reply = job.reply.clone();
    if job.check_only {
        let _ = reply.send(ToolOutput::text("allowed"));
    } else if let Some(out) = start(world, job) {
        let _ = reply.send(out);
    }
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
    let mut v = json!({
        "name": o.name,
        "asset": o.asset.to_string_lossy().replace('\\', "/"),
        "position": [r2(t.translation.x), r2(t.translation.y), r2(t.translation.z)],
        "yaw_deg": r2(yaw.to_degrees()),
        "scale": r2(t.scale.x),
        "world_size_m": [r2(mx.x - mn.x), r2(mx.y - mn.y), r2(mx.z - mn.z)],
    });
    if o.locked {
        v["locked"] = json!(true);
    }
    v
}

/// The user locked this object: tools leave its placement alone.
fn is_locked(world: &World, e: Entity) -> bool {
    world.get::<PlacedObject>(e).is_some_and(|o| o.locked)
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
                out = ToolOutput::text(format!("No assets match '{query}'. Try other words or list_asset_categories. If the library really has none, make it with create_asset (or a custom_ tool) rather than leaving it out."));
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
                Some(e) if is_locked(world, e) && ["x", "y", "z", "yaw_deg", "scale"].iter().any(|k| f(i, k).is_some()) => {
                    ToolOutput::err(format!("'{name}' is locked: ask the user to unlock it before moving it"))
                }
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
            let mut locked = Vec::new();
            for n in i["names"].as_array().into_iter().flatten().filter_map(|v| v.as_str()) {
                match find_object(world, n) {
                    Some(e) if is_locked(world, e) => locked.push(n.to_string()),
                    Some(e) => {
                        world.resource_mut::<Actions>().0.push(Action::Delete(e));
                        done.push(n.to_string());
                    }
                    None => missing.push(n.to_string()),
                }
            }
            ToolOutput::ok(json!({ "deleted": done, "not_found": missing, "locked_not_deleted": locked }))
        }
        "clear_scene" => {
            world.resource_mut::<Actions>().0.push(Action::ClearScene);
            ToolOutput::text("scene cleared")
        }
        "load_template" => {
            let id = i["name"].as_str().unwrap_or_default();
            match crate::samples::write_template(id, world.resource::<Library>()) {
                Ok((path, downloading)) => {
                    let doc: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
                    let names: Vec<&str> = doc["objects"].as_array().into_iter().flatten().filter_map(|o| o["name"].as_str()).collect();
                    let devices: Vec<&str> = doc["rtls"]["devices"].as_array().into_iter().flatten().filter_map(|d| d["id"].as_str()).collect();
                    let zones: Vec<&str> = doc["rtls"]["zones"].as_array().into_iter().flatten().filter_map(|z| z["name"].as_str()).collect();
                    let out = json!({
                        "loaded": id,
                        "objects": names,
                        "rtls_devices": devices,
                        "zones": zones,
                        "downloading": downloading,
                        "note": "objects spawn over the next frames; call get_scene before moving them",
                    });
                    world.resource_mut::<Actions>().0.push(Action::LoadLayout(path));
                    ToolOutput::ok(out)
                }
                Err(e) => ToolOutput::err(format!("{e:#}")),
            }
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
        "screenshot" => return defer(world, Wait::Screenshot { slot: None, settled: 0, reply, started: Instant::now(), compare: None }),
        "compare_to_photo" => {
            let id = i["image_id"].as_str().unwrap_or("").to_string();
            let cam = super::vision::perceive::scenes().lock().unwrap().get(&id).map(|sc| (sc.camera.eye(), sc.camera.forward(), sc.hfov_deg, sc.width as f32 / sc.height.max(1) as f32));
            let Some((eye, forward, hfov, photo_aspect)) = cam else {
                return Some(ToolOutput::err(format!("no perceive_photo result for '{id}'")));
            };
            let view_aspect = {
                let mut q = world.query::<(&Camera, &OrbitCamera)>();
                q.iter(world).next().and_then(|(c, _)| c.logical_viewport_size()).map(|s| s.x / s.y.max(1.0)).unwrap_or(16.0 / 9.0)
            };
            set_camera(world, eye, eye + forward * 10.0, Some(photocheck::fit_hfov(hfov, photo_aspect, view_aspect)));
            return defer(world, Wait::Screenshot { slot: None, settled: 0, reply, started: Instant::now(), compare: Some(id) });
        }
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
                approval: false,
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
            // Nothing gets a made-up position.
            {
                let scenes = super::vision::perceive::scenes().lock().unwrap();
                if let Some(sc) = scenes.get(&image) {
                    for it in i["items"].as_array().into_iter().flatten() {
                        let det = it["id"].as_u64().unwrap_or(u64::MAX) as usize;
                        if sc.objects.get(det).is_some_and(|o| o.footprint.is_none()) {
                            errors.push(format!("detection {det} has no position yet: give it height_m, width_m, base_y or at_xz with set_photo_layout"));
                        }
                    }
                }
            }
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
        "add_structure" if i["kind"].as_str() == Some("glass_door") => {
            let (width, height) = vec3(&i["size_m"]).map_or(assetgen::GLASS_DOOR_M, |s| (s.x, s.y));
            match assetgen::glass_door(width, height) {
                Ok(door) => {
                    world.resource_mut::<Catalog>().dims.insert(door.path.to_string_lossy().replace('\\', "/"), door.size.to_array());
                    world.resource_mut::<Library>().rescan();
                    world.resource_mut::<PrototypeCache>().request(&door.path);
                    let transform = Transform::from_xyz(f(i, "x").unwrap_or(0.0), f(i, "y").unwrap_or(0.0), f(i, "z").unwrap_or(0.0)).with_rotation(Quat::from_rotation_y(f(i, "yaw_deg").unwrap_or(0.0).to_radians()));
                    let items = vec![PlaceItem { usd: door.path, transform, name: i["name"].as_str().filter(|s| !s.is_empty()).map(str::to_string), ..Default::default() }];
                    return defer(world, Wait::Place { items, reply, started: Instant::now(), extra: Value::Null });
                }
                Err(e) => ToolOutput::err(format!("{e:#}")),
            }
        }
        "add_structure" => {
            use super::structures::{self, Kind, Piece};
            let mut extra = Value::Null;
            let pieces: Vec<Piece> = if let Some(img) = i["from_photo"].as_str() {
                match super::vision::perceive::scenes().lock().unwrap().get_mut(img) {
                    Some(sc) => {
                        if let Some(c) = i["ceiling"].as_bool() {
                            sc.no_ceiling = !c;
                        }
                        if let (Some(l), Some(r), Some(d)) = (f(&i["room"], "left_m"), f(&i["room"], "right_m"), f(&i["room"], "depth_m")) {
                            sc.room = Some([-l.abs(), -d.abs(), r.abs(), 0.0]);
                        }
                        // The room is an estimate to make, not something to read off the photo.
                        if sc.room.is_none() {
                            return Some(ToolOutput::err("give room {left_m, right_m, depth_m}: your estimate of the whole room from the camera"));
                        }
                        // A stated height beats anything measured from the photo.
                        let stated = f(i, "height_m");
                        if let Some(h) = stated {
                            sc.ceiling_y = Some(h);
                        }
                        let mut shell = structures::shell_from_photo(sc);
                        // The photo itself, projected onto floor, ceiling and walls.
                        if i["project_photo"].as_bool().unwrap_or(true)
                            && let Some(photo) = images::get(img).and_then(|(d, _)| image::load_from_memory(&d).ok()).map(|d| d.to_rgb8())
                        {
                            for (n, p) in shell.iter_mut().enumerate() {
                                if !matches!(p.kind, Kind::Floor | Kind::Ceiling | Kind::Wall) || !i["materials"][p.kind.name()].is_null() {
                                    continue;
                                }
                                let fill = match p.kind {
                                    Kind::Floor => sc.colors.floor,
                                    Kind::Ceiling => sc.colors.ceiling,
                                    _ => sc.colors.walls,
                                }
                                .unwrap_or([180, 180, 180]);
                                if let Some(tex) = structures::project_photo(sc, &photo, p, fill)
                                    && let Ok(m) = structures::projected_material(&tex, &format!("{img}_{n}_{}x{}x{:.2}", tex.width(), tex.height(), p.base_y))
                                {
                                    p.material = Some(m);
                                }
                            }
                        }
                        if i["with_beams"].as_bool().unwrap_or(false) {
                            shell.extend(structures::beams_for_photo(sc, f(i, "beam_spacing_m").unwrap_or(6.0)));
                        }
                        for p in &mut shell {
                            let spec = &i["materials"][p.kind.name()];
                            if !spec.is_null() {
                                match assetgen::Mat::parse(spec, p.material.clone().unwrap_or_default()) {
                                    Ok(m) => p.material = Some(m),
                                    Err(e) => return Some(ToolOutput::err(format!("materials.{}: {e:#}", p.kind.name()))),
                                }
                            }
                        }
                        if !sc.no_ceiling {
                            let (h, source) = structures::ceiling_height(sc);
                            extra = json!({ "ceiling_height_m": r2(h), "ceiling_source": if stated.is_some() { "as you said" } else { source } });
                        }
                        shell
                    }
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
                    material: None,
                }]
            };
            // A material on a single call applies to what that call makes.
            let mut pieces = pieces;
            if !i["material"].is_null() {
                match assetgen::Mat::parse(&i["material"], assetgen::Mat::default()) {
                    Ok(m) => {
                        for p in pieces.iter_mut().filter(|p| p.kind != Kind::Beam) {
                            p.material = Some(m.clone());
                        }
                    }
                    Err(e) => return Some(ToolOutput::err(format!("material: {e:#}"))),
                }
            }
            if pieces.is_empty() {
                return Some(ToolOutput::text("nothing to build"));
            }
            let mut items = Vec::new();
            for p in &pieces {
                let usd = match &p.material {
                    Some(m) => structures::usd_with_material(p.kind, p.size, m),
                    None => structures::usd_for(p.kind, p.size),
                };
                match usd {
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
            return defer(world, Wait::Place { items, reply, started: Instant::now(), extra });
        }
        "create_asset" => match assetgen::create(i) {
            Ok(c) => {
                if c.replaced {
                    world.resource_mut::<PrototypeCache>().states.remove(&c.path);
                }
                let key = c.path.to_string_lossy().replace('\\', "/");
                world.resource_mut::<Catalog>().dims.insert(key.clone(), c.size.to_array());
                world.resource_mut::<Library>().rescan();
                let mut v = json!({
                    "asset": c.slug,
                    "path": key,
                    "size_m": [r2(c.size.x), r2(c.size.y), r2(c.size.z)],
                    "parts": c.parts,
                });
                if c.replaced {
                    v["note"] = json!("replaced the earlier asset of this name; copies already placed keep the old look until the layout reloads");
                }
                ToolOutput::ok(v)
            }
            Err(e) => ToolOutput::err(format!("{e:#}")),
        },
        "set_object_material" => {
            let name = i["name"].as_str().unwrap_or("");
            let Some(e) = find_object(world, name) else {
                return Some(ToolOutput::err(format!("no object named '{name}'")));
            };
            // A new material means replacing the object.
            if is_locked(world, e) {
                return Some(ToolOutput::err(format!("'{name}' is locked: ask the user to unlock it first")));
            }
            let (asset, transform) = {
                let em = world.entity(e);
                (em.get::<PlacedObject>().unwrap().asset.clone(), *em.get::<Transform>().unwrap())
            };
            match assetgen::reskin(&asset, i) {
                Ok(usd) => {
                    world.resource_mut::<Actions>().0.push(Action::Delete(e));
                    world.resource_mut::<PrototypeCache>().request(&usd);
                    let items = vec![PlaceItem { usd, transform, name: Some(name.to_string()), ..Default::default() }];
                    return defer(world, Wait::Place { items, reply, started: Instant::now(), extra: Value::Null });
                }
                Err(e) => ToolOutput::err(format!("{e:#}")),
            }
        }
        "set_photo_layout" => {
            let id = i["image_id"].as_str().unwrap_or("").to_string();
            let Some((data, _)) = images::get(&id) else {
                return Some(ToolOutput::err(format!("unknown image_id '{id}'")));
            };
            match super::vision::perceive::set_layout(&id, &data, &layout_guess(i)) {
                Ok(scene) => photo_scene_output(&scene, &data),
                Err(e) => ToolOutput::err(format!("{e:#}")),
            }
        }
        "check_against_depth" => {
            let id = i["image_id"].as_str().unwrap_or("").to_string();
            let Some((data, _)) = images::get(&id) else {
                return Some(ToolOutput::err(format!("unknown image_id '{id}'")));
            };
            let Some(sc) = super::vision::perceive::scenes().lock().unwrap().get(&id).cloned() else {
                return Some(ToolOutput::err(format!("no perceive_photo result for '{id}'")));
            };
            let (eye, forward) = (sc.camera.eye(), sc.camera.forward());
            let mut q = world.query::<(&PlacedObject, &Transform)>();
            let items: Vec<super::vision::perceive::LayoutItem> = q
                .iter(world)
                .filter_map(|(o, t)| {
                    let det = sc.objects.get(*sc.placed.get(&o.name)?)?;
                    let (mn, mx) = world_bounds(o, t);
                    // The nearest face is what the depth model sees.
                    let distance_m = [mn.x, mx.x]
                        .iter()
                        .flat_map(|x| [mn.z, mx.z].map(|z| (Vec3::new(*x, eye.y, z) - eye).dot(forward)))
                        .fold(f32::MAX, f32::min);
                    Some(super::vision::perceive::LayoutItem { name: o.name.clone(), bbox: det.bbox, distance_m })
                })
                .collect();
            // Inference takes seconds.
            std::thread::spawn(move || {
                let out = match super::vision::perceive::check_depth(&id, &data, &items) {
                    Ok(v) => ToolOutput::ok(v),
                    Err(e) => ToolOutput::err(format!("depth check unavailable: {e:#}")),
                };
                let _ = reply.send(out);
            });
            return None;
        }
        "add_photo_backdrop" => {
            let image = i["image_id"].as_str().unwrap_or("");
            let b: Vec<f32> = i["box"].as_array().into_iter().flatten().filter_map(|v| v.as_f64().map(|x| x as f32)).collect();
            let Some(distance) = f(i, "distance_m").filter(|d| *d > 0.1) else {
                return Some(ToolOutput::err("distance_m must be positive"));
            };
            if b.len() != 4 || b[2] <= b[0] || b[3] <= b[1] {
                return Some(ToolOutput::err("box must be [x0, y0, x1, y1] with x1 > x0 and y1 > y0"));
            }
            let spot = super::vision::perceive::scenes().lock().unwrap().get(image).map(|sc| super::placer::backdrop(sc, [b[0], b[1], b[2], b[3]], distance));
            let Some(spot) = spot else {
                return Some(ToolOutput::err(format!("no perceive_photo result for '{image}'")));
            };
            let name = i["name"].as_str().filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| format!("backdrop_{image}_{}", (b[0] * 100.0) as u32 * 1000 + (b[1] * 100.0) as u32));
            let spec = json!({
                "name": name,
                "mount": "wall",
                "description": assetgen::PHOTO_BACKDROP,
                "parts": [{ "shape": "panel", "size": [spot.width, spot.height, 0.001], "material": { "photo": { "image_id": image, "box": b } } }]
            });
            match assetgen::create(&spec) {
                Ok(c) => {
                    if c.replaced {
                        world.resource_mut::<PrototypeCache>().states.remove(&c.path);
                    }
                    world.resource_mut::<Library>().rescan();
                    world.resource_mut::<PrototypeCache>().request(&c.path);
                    let base = Vec3::new(spot.center.x, spot.center.y - spot.height * 0.5, spot.center.z);
                    let items = vec![PlaceItem {
                        usd: c.path,
                        transform: Transform::from_translation(base).with_rotation(Quat::from_rotation_y(spot.yaw)),
                        name: Some(c.slug),
                        ..Default::default()
                    }];
                    return defer(world, Wait::Place { items, reply, started: Instant::now(), extra: json!({ "width_m": r2(spot.width), "height_m": r2(spot.height) }) });
                }
                Err(e) => ToolOutput::err(format!("{e:#}")),
            }
        }
        "define_tool" => match toolmaker::define(i, &tool_defs(false)) {
            Ok(n) => ToolOutput::text(format!("saved; call it as {n} from your next turn")),
            Err(e) => ToolOutput::err(format!("{e:#}")),
        },
        "delete_tool" => match toolmaker::delete(i["name"].as_str().unwrap_or("")) {
            Ok(()) => ToolOutput::text("deleted"),
            Err(e) => ToolOutput::err(format!("{e:#}")),
        },
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
                // Whatever name it's given, a project is a project file.
                let p = if job.name == "save_layout" { crate::project_file::save_path(&p) } else { p };
                let action = if job.name == "save_layout" { Action::SaveLayout(p.clone()) } else { Action::ExportUsda(p.clone()) };
                world.resource_mut::<Actions>().0.push(action);
                ToolOutput::text(format!("writing {}", p.display()))
            }
            None => ToolOutput::err("path is required"),
        },
        // Reached through open_file, which only the assistant's own loop has.
        "open_layout_tab" => match i["path"].as_str() {
            Some(p) => {
                let p = PathBuf::from(p);
                if crate::project_file::holds_antenna(&p) {
                    let text = format!("opening {} in Antenna, in place of the antenna that was showing", p.display());
                    crate::ui::open_antenna(world, p);
                    return Some(ToolOutput::text(text));
                }
                let tab = crate::tabs::NewTab { title: crate::tabs::title_of(&p), path: Some(p.clone()), load: Some(p.clone()) };
                world.resource_mut::<crate::tabs::Tabs>().request(crate::tabs::Request::Open(vec![tab]));
                ToolOutput::text(format!("opening {} in a new tab; the scene that was open stays in its own tab", p.display()))
            }
            None => ToolOutput::err("path is required"),
        },
        "get_screen" => match world.resource::<Agent>().screen_note() {
            Some(note) => ToolOutput::text(note),
            None => ToolOutput::text("The user has turned off sharing what is on screen. Ask them if you need to know."),
        },
        "load_reference_image" => match images::load_file(Path::new(i["path"].as_str().unwrap_or(""))) {
            Ok((id, data, mime)) => ToolOutput {
                content: vec![Block::Text(format!("image_id: {id}")), Block::Image { data, mime }],
                is_error: false,
            },
            Err(e) => ToolOutput::err(e),
        },
        other if other.starts_with(toolmaker::PREFIX) => {
            // Its steps come back through this bridge, so it can't block the frame.
            let (name, input, tx, ctx) = (other.to_string(), i.clone(), world.resource::<ToolBridge>().tx.clone(), job.ctx.clone());
            std::thread::spawn(move || {
                let _ = reply.send(toolmaker::run(&name, &input, tx, ctx));
            });
            return None;
        }
        "site_import_bim" => return import_bim(world, i, reply),
        // Before the outcome recipes, which claim the rest of the `design_` prefix.
        other if crate::sim::tools::is_tool(other) => crate::sim::tools::start(world, other, i),
        other if crate::outcomes::tools::is_tool(other) => match crate::outcomes::tools::start(world, other, i) {
            Some(out) => out,
            None => ToolOutput::err(format!("unknown tool '{other}'")),
        },
        other if other.starts_with("plant_") => {
            let s = crate::plant::tools::start(world, other, i);
            return run_started(world, s, reply);
        }
        other if other.starts_with("rtls_") => match crate::rtls::tools::start(world, other, i) {
            Some(out) => out,
            None => ToolOutput::err(format!("unknown tool '{other}'")),
        },
        #[cfg(feature = "antenna-studio")]
        other if other.starts_with("antenna_") => match crate::antenna::tools::start(world, other, i) {
            Some(out) => out,
            None => ToolOutput::err(format!("unknown tool '{other}'")),
        },
        other if super::knowledge::is_tool(other) => super::knowledge::run(other, i),
        // External MCP servers answer over the network, so never on this thread.
        other if world.get_resource::<Agent>().is_some_and(|a| a.mcp.has_tool(other)) => {
            let (hub, name, input) = (world.resource::<Agent>().mcp.clone(), other.to_string(), i.clone());
            std::thread::spawn(move || {
                let _ = reply.send(hub.call(&name, &input).unwrap_or_else(|| ToolOutput::err(format!("the MCP server for '{name}' has gone"))));
            });
            return None;
        }
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
        Wait::Approval { job, answer, remember } => {
            let said = match answer.try_recv() {
                Ok(out) => out.summary(),
                Err(crossbeam_channel::TryRecvError::Empty) if !job.ctx.cancelled() => return Some(Wait::Approval { job, answer, remember }),
                // New chat dropped the card, or Stop was pressed: nobody said yes.
                Err(_) => String::new(),
            };
            let once = said == format!("User answered: {ALLOW}");
            let always = remember.as_ref().filter(|(label, _)| said == format!("User answered: {label}"));
            if let Some((_, key)) = always {
                world.resource_mut::<Agent>().approved.insert(key.clone());
            }
            if once || always.is_some() {
                go_ahead(world, job);
            } else {
                let _ = job.reply.send(ToolOutput::err(format!("The user did not allow {}. Don't try to reach the same result another way; say what you wanted to do and why.", job.name)));
            }
            None
        }
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
        Wait::Screenshot { slot: None, settled, reply, started, compare } => {
            // Keep the UI overlays (properties card, toolbar…) out of the capture.
            if let Some(mut h) = world.get_resource_mut::<crate::ui::HideOverlays>() {
                h.0 = h.0.max(4);
            }
            // New assets need a few frames for meshes and textures to reach the GPU.
            let fresh = world.resource::<PendingTools>().1.is_some_and(|t| t.elapsed() < Duration::from_secs(3));
            let busy = fresh || world.resource::<PrototypeCache>().loading_count() > 0;
            let settled = if busy { 0 } else { settled + 1 };
            if settled < 10 && started.elapsed() < Duration::from_secs(60) {
                return Some(Wait::Screenshot { slot: None, settled, reply, started, compare });
            }
            let slot: Arc<Mutex<Option<Image>>> = Arc::default();
            let s = slot.clone();
            world.spawn(Screenshot::primary_window()).observe(move |ev: On<ScreenshotCaptured>| {
                *s.lock().unwrap() = Some(ev.image.clone());
            });
            Some(Wait::Screenshot { slot: Some(slot), settled, reply, started: Instant::now(), compare })
        }
        Wait::Screenshot { slot: Some(slot), settled, reply, started, compare } => {
            let img = slot.lock().unwrap().take();
            match img {
                Some(img) => {
                    let out = screenshot_output(world, img);
                    let _ = reply.send(match compare {
                        Some(id) => compare_output(world, &id, out),
                        None => out,
                    });
                    None
                }
                None if started.elapsed() > Duration::from_secs(10) => {
                    let _ = reply.send(ToolOutput::err("screenshot timed out"));
                    None
                }
                None => Some(Wait::Screenshot { slot: Some(slot), settled, reply, started, compare }),
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
                            // A stand-in's front is a piece of the photo: it goes exactly where
                            // the photo camera sees that piece, so edges meet the photo's.
                            if let Some((gw, gh)) = assetgen::standin_guess(&it.usd)
                                && let Some(spot) = super::placer::standin_spot(sc, &obj, gw, gh)
                            {
                                let size = p.aabb_max - p.aabb_min;
                                let s = spot.width / size.x.max(1e-3);
                                let rot = Quat::from_rotation_y(spot.yaw);
                                let to_face = Vec3::new(0.0, spot.height * 0.5, p.aabb_max.z * s);
                                t = Transform::from_translation(spot.center - rot * to_face).with_rotation(rot).with_scale(Vec3::splat(s));
                            }
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

/// The assistant's hypothesis from a tool call.
fn layout_guess(i: &Value) -> super::vision::perceive::LayoutGuess {
    use super::vision::perceive::{CameraGuess, LayoutGuess, ObjectGuess};
    let c = &i["camera"];
    let camera = f(c, "height_m").map(|height_m| {
        let level = f(c, "horizon_v").unwrap_or(0.5);
        CameraGuess { height_m, horizon_left_v: f(c, "horizon_left_v").unwrap_or(level), horizon_right_v: f(c, "horizon_right_v").unwrap_or(level) }
    });
    let objects = i["objects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            let at = o["at_xz"].as_array().filter(|a| a.len() == 2).and_then(|a| Some([a[0].as_f64()? as f32, a[1].as_f64()? as f32]));
            Some((o["id"].as_u64()? as usize, ObjectGuess { height_m: f(o, "height_m"), width_m: f(o, "width_m"), base_y: f(o, "base_y"), at_xz: at, yaw_deg: f(o, "yaw_deg") }))
        })
        .collect();
    LayoutGuess { camera, hfov_deg: f(i, "hfov_deg"), building_axis_deg: f(i, "building_axis_deg"), objects }
}

/// A photo scene as a tool result: its JSON and the overlay.
fn photo_scene_output(scene: &super::vision::perceive::PhotoScene, data: &[u8]) -> ToolOutput {
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
            out.content.push(Block::Text("Detections (green, numbered by id) and your horizon (cyan):".into()));
            out.content.push(png_block(png));
        }
    }
    out
}

/// Sets a viewport capture against the photo it should resemble.
fn compare_output(world: &mut World, image_id: &str, shot: ToolOutput) -> ToolOutput {
    if shot.is_error {
        return shot;
    }
    let render = shot.content.iter().find_map(|b| if let Block::Image { data, .. } = b { image::load_from_memory(data).ok() } else { None });
    let photo = images::get(image_id).and_then(|(data, _)| image::load_from_memory(&data).ok());
    let (Some(render), Some(photo)) = (render, photo) else { return ToolOutput::err("could not read the photo or the render") };
    let (img, metrics) = photocheck::compare(&photo.to_rgb8(), &render.to_rgb8());
    let Some(png) = encode_png(&image::DynamicImage::ImageRgb8(img)) else { return ToolOutput::err("could not encode the comparison") };
    let data = Arc::new(png);
    let id = images::put(data.clone(), "image/png");
    world.resource_mut::<Agent>().transcript.push(ChatItem::Image(id));
    let mut metrics = metrics;
    if let Some(sc) = super::vision::perceive::scenes().lock().unwrap().get(image_id) {
        let (eye, target) = (sc.camera.eye(), sc.camera.eye() + sc.camera.forward() * 10.0);
        metrics["camera"] = json!({ "eye": [eye.x, eye.y, eye.z], "target": [target.x, target.y, target.z] });
    }
    ToolOutput { content: vec![Block::Text(pretty(&metrics)), Block::Image { data, mime: "image/png".into() }], is_error: false }
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
    let layout = layout_guess(i);
    let req = perceive::Request {
        image_id,
        bytes: data,
        labels: &labels,
        extra_boxes: &extra,
        default_hfov,
        layout: &layout,
        threshold: f(i, "threshold").unwrap_or(0.2).clamp(0.05, 0.9),
    };
    let scene = match perceive::perceive(&req) {
        Ok(s) => s,
        Err(e) => {
            return ToolOutput::err(format!(
                "{e:#}\nThe detector is unavailable for this photo: give the objects you see as extra_boxes with no labels."
            ));
        }
    };
    let _ = default_hfov;
    photo_scene_output(&scene, data)
}

// ---------------------------------------------------------------------------
// Buildings from BIM

fn bim_tool_def() -> ToolDef {
    ToolDef {
        name: "site_import_bim".into(),
        description: "Build the scene's structure from a BIM file (IFC): walls, slabs, roofs, columns, beams, doors and windows, each with the radio material its BIM material implies, for predicting coverage of a building that doesn't exist yet. Reports what was placed and what it couldn't read.".into(),
        schema: json!({ "type": "object", "properties": { "path": { "type": "string" }, "skip": { "type": "array", "items": { "type": "string" }, "description": "Element kinds to leave out, e.g. [\"roof\"] so the inside stays visible" }, "include_proxies": { "type": "boolean", "description": "Also place generic proxies such as trees and markers (default false)" } }, "required": ["path"] }),
    }
}

/// IFC is Z up in metres; the scene is Y up, with IFC's Y running along −Z.
pub(crate) fn pieces_from_building(b: &ifc::Building, skip: &[String], include_other: bool) -> Vec<(super::structures::Piece, String, (crate::rtls::radio::Material, f32))> {
    use super::structures::{Kind, Piece};
    let boxes = crate::outcomes::spec::boxes_from_building(b, skip, include_other);
    let ground = crate::outcomes::spec::ground_level(&boxes);
    boxes
        .into_iter()
        .map(|x| {
            let kind = match x.kind {
                ifc::ElementKind::Wall | ifc::ElementKind::Covering => Kind::Wall,
                ifc::ElementKind::Column => Kind::Column,
                ifc::ElementKind::Beam => Kind::Beam,
                ifc::ElementKind::Slab => Kind::Floor,
                ifc::ElementKind::Roof => Kind::Ceiling,
                ifc::ElementKind::Door | ifc::ElementKind::Window | ifc::ElementKind::Other => Kind::Block,
            };
            // Along its X, height, along its Y.
            let size = Vec3::new(x.size[0].max(0.01) as f32, x.size[2].max(0.01) as f32, x.size[1].max(0.01) as f32);
            let piece = Piece { kind, size, center: Vec2::new(x.center[0] as f32, -(x.center[1] as f32)), base_y: (x.center[2] - x.size[2] / 2.0 - ground) as f32, yaw: x.yaw as f32, material: None };
            (piece, x.name, x.material)
        })
        .collect()
}

fn import_bim(world: &mut World, i: &Value, reply: Sender<ToolOutput>) -> Option<ToolOutput> {
    let Some(path) = i["path"].as_str() else { return Some(ToolOutput::err("give the path of an .ifc file")) };
    let building = match ifc::read_file(std::path::Path::new(path)) {
        Ok(b) => b,
        Err(e) => return Some(ToolOutput::err(format!("{path}: {e:#}"))),
    };
    let skip: Vec<String> = i["skip"].as_array().map_or(Vec::new(), |a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect());
    let pieces = pieces_from_building(&building, &skip, i["include_proxies"].as_bool().unwrap_or(false));
    if pieces.is_empty() {
        return Some(ToolOutput::err(format!("{path}: nothing to place ({} entities skipped)", building.skipped.len())));
    }
    let mut items = Vec::with_capacity(pieces.len());
    let glass = assetgen::Mat::preset("glass").unwrap_or_default();
    for (p, name, material) in &pieces {
        // What radio sees as glass should look like glass, not a grey block.
        let usd = if material.0 == crate::rtls::radio::Material::Glass { super::structures::usd_with_material(p.kind, p.size, &glass) } else { super::structures::usd_for(p.kind, p.size) };
        match usd {
            Ok(usd) => {
                crate::rtls::radio::set_object_material(name, Some(*material));
                items.push(PlaceItem {
                    usd,
                    transform: Transform::from_xyz(p.center.x, p.base_y, p.center.y).with_rotation(Quat::from_rotation_y(p.yaw)),
                    name: Some(name.clone()),
                    on_top_of: None,
                    photo: None,
                    ..Default::default()
                });
            }
            Err(e) => return Some(ToolOutput::err(format!("could not write structure: {e}"))),
        }
    }
    let mut cache = world.resource_mut::<PrototypeCache>();
    for it in &items {
        cache.request(&it.usd);
    }
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for (kind, n) in building.counts() {
        counts.insert(format!("{kind:?}"), n);
    }
    let extra = json!({ "building": building.name, "read": counts, "skipped": building.skipped.len(), "skipped_examples": building.skipped.iter().take(5).collect::<Vec<_>>(), "approximated": building.elements.iter().filter(|e| !e.exact).count() });
    defer(world, Wait::Place { items, reply, started: Instant::now(), extra })
}

#[cfg(test)]
mod lock_tests {
    use super::*;
    use crate::scene::lock_tests::object;

    fn tool(world: &mut World, name: &str, input: Value) -> ToolOutput {
        let (reply, _rx) = unbounded();
        start(world, ToolJob { name: name.into(), input, reply, ctx: CallCtx::default(), check_only: false }).expect("finishes at once")
    }

    /// One client's `site_trace` calls filled the chat, burying the conversation.
    #[test]
    fn a_call_from_an_mcp_client_is_answered_and_stays_out_of_the_chat() {
        let mut w = World::new();
        w.insert_resource(Agent::new(Default::default()));
        w.init_resource::<ToolBridge>();
        w.init_resource::<PendingTools>();
        let (reply, answer) = unbounded();
        let tx = w.resource::<ToolBridge>().tx.clone();
        let ctx = CallCtx { origin: Origin::Mcp, ..CallCtx::default() };
        tx.send(ToolJob { name: "no_such_tool".into(), input: json!({ "from": [0.0, 1.2, 0.0] }), reply, ctx, check_only: false }).unwrap();
        run_tool_jobs(&mut w);
        answer.try_recv().expect("the client still gets its answer");
        assert!(w.resource::<Agent>().transcript.is_empty());
    }

    #[test]
    fn a_locked_object_refuses_to_move_but_can_be_renamed() {
        let mut w = World::new();
        let e = object(&mut w, "kept", true);
        let before = *w.get::<Transform>(e).unwrap();
        let out = tool(&mut w, "update_object", json!({ "name": "kept", "x": 9.0 }));
        assert!(out.is_error && out.summary().contains("locked"), "{}", out.summary());
        assert_eq!(*w.get::<Transform>(e).unwrap(), before);
        let out = tool(&mut w, "update_object", json!({ "name": "kept", "new_name": "still_kept" }));
        assert!(!out.is_error, "{}", out.summary());
        assert_eq!(w.get::<PlacedObject>(e).unwrap().name, "still_kept");
    }

    #[test]
    fn delete_keeps_locked_objects_and_says_so() {
        let mut w = World::new();
        w.init_resource::<Actions>();
        object(&mut w, "kept", true);
        let free = object(&mut w, "free", false);
        let out = tool(&mut w, "delete_objects", json!({ "names": ["kept", "free"] }));
        let v: Value = serde_json::from_str(&out.summary()).unwrap();
        assert_eq!(v["deleted"], json!(["free"]));
        assert_eq!(v["locked_not_deleted"], json!(["kept"]));
        assert!(matches!(w.resource::<Actions>().0.as_slice(), [Action::Delete(e)] if *e == free));
    }

    #[test]
    fn a_new_material_is_refused_because_it_would_replace_the_object() {
        let mut w = World::new();
        object(&mut w, "kept", true);
        let out = tool(&mut w, "set_object_material", json!({ "name": "kept" }));
        assert!(out.is_error && out.summary().contains("locked"), "{}", out.summary());
    }

    #[test]
    fn the_scene_listing_shows_the_lock() {
        let mut w = World::new();
        object(&mut w, "kept", true);
        object(&mut w, "free", false);
        let v: Value = serde_json::from_str(&tool(&mut w, "get_scene", json!({})).summary()).unwrap();
        let of = |n: &str| v["objects"].as_array().unwrap().iter().find(|o| o["name"] == n).unwrap()["locked"].clone();
        assert_eq!(of("kept"), json!(true));
        assert_eq!(of("free"), Value::Null);
    }
}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod gate_tests;

#[cfg(test)]
mod routing_tests {
    use super::*;

    /// Runs the real dispatch on an empty world. A handler that wants a missing resource panics,
    /// which still proves the call reached it; only a fall through every arm says "unknown tool".
    fn reaches_a_handler(name: &str) -> bool {
        let mut world = World::new();
        let (tx, _rx) = unbounded();
        let job = ToolJob { name: name.into(), input: json!({}), reply: tx, ctx: CallCtx::default(), check_only: false };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| start(&mut world, job))) {
            Ok(Some(out)) => !(out.is_error && out.summary().starts_with("unknown tool")),
            Ok(None) | Err(_) => true,
        }
    }

    #[test]
    fn every_listed_tool_reaches_a_handler() {
        assert!(!reaches_a_handler("no_such_tool"), "the check must be able to fail");
        let lost: Vec<String> = tool_defs(true).into_iter().map(|d| d.name).filter(|n| !reaches_a_handler(n)).collect();
        assert!(lost.is_empty(), "listed but unreachable: {lost:?}");
    }

    /// All six failed from the assistant: the outcome recipes claimed the `design_` prefix first.
    #[test]
    fn sim_tools_arent_taken_for_outcome_recipes() {
        for name in ["sim_faces", "sim_catalog", "sim_run"] {
            assert!(crate::sim::tools::is_tool(name), "{name}");
            assert!(tool_defs(false).iter().any(|d| d.name == name), "{name} is still offered");
            assert!(reaches_a_handler(name), "{name}");
        }
        for name in ["design_job", "design_shelf", "design_tag", "site_trace"] {
            assert!(!crate::sim::tools::is_tool(name), "{name}");
        }
    }
}
