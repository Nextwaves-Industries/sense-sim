//! Scans a SimReady dataset folder, merges it with the upstream catalog, loads
//! thumbnails (local, or fetched and cached for online-only assets), and
//! downloads missing assets on demand through a small queue.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy_egui::egui;
use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::remote::{self, Progress};

const AUTO_RESCAN: Duration = Duration::from_secs(15);
const MAX_ACTIVE_DOWNLOADS: usize = 2;
const THUMB_WORKERS: usize = 6;

#[derive(Clone)]
pub struct LibraryEntry {
    /// Display name, e.g. `TV_a01` (or `folder / stem` for multi-USD folders).
    pub name: String,
    /// Asset folder name in the dataset.
    pub folder: String,
    /// Root USD file; `None` while the asset is not (completely) on disk.
    pub usd: Option<PathBuf>,
    pub thumb: Option<egui::TextureHandle>,
    /// The asset defines joints (doors, lids, drawers…), known for on-disk assets.
    pub movable: bool,
}

pub enum DownloadState {
    Queued,
    Active { done: u64, total: u64 },
}

struct Download {
    progress: Option<Arc<Progress>>,
    place_when_done: bool,
}

/// Where a thumbnail comes from.
#[derive(Clone, PartialEq, Eq, Hash)]
enum ThumbKey {
    File(PathBuf),
    Online(String),
}

#[derive(Resource)]
pub struct Library {
    pub root: String,
    pub entries: Vec<LibraryEntry>,
    pub search: String,
    pub show_remote: bool,
    pub remote_status: String,
    remote: Vec<String>,
    remote_rx: Option<Receiver<Result<Vec<String>, String>>>,
    entry_thumb_keys: Vec<Option<ThumbKey>>,
    thumbs: HashMap<ThumbKey, Option<egui::TextureHandle>>,
    thumbs_requested: HashSet<ThumbKey>,
    thumbs_tx: Sender<(ThumbKey, Option<egui::ColorImage>)>,
    thumbs_rx: Receiver<(ThumbKey, Option<egui::ColorImage>)>,
    online_thumb_queue: Sender<String>,
    downloads: HashMap<String, Download>,
    download_queue: VecDeque<String>,
    last_scan: Instant,
    /// Whether each root USD has joints; reading the physics file once is enough.
    movable: HashMap<PathBuf, bool>,
}

fn config_path() -> Option<PathBuf> {
    let base = crate::app_paths::config_base()?;
    Some(base.join("SenseSim").join("config.json"))
}

fn cache_dir() -> PathBuf {
    let base = crate::app_paths::data_base();
    base.join("SenseSim").join("thumbs")
}

/// Picks the dataset folder: explicit argument, last used, `D:/simready-assets`,
/// or a folder in the user profile (created on demand).
pub fn default_root(arg: Option<String>) -> String {
    if let Some(a) = arg {
        return a;
    }
    if let Some(p) = config_path()
        && let Ok(txt) = std::fs::read_to_string(p)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt)
        && let Some(r) = v["root"].as_str()
        && Path::new(r).is_dir()
    {
        return r.to_string();
    }
    if cfg!(windows) && Path::new("D:/simready-assets").is_dir() {
        return "D:/simready-assets".into();
    }
    let home = crate::app_paths::home().unwrap_or_else(|| ".".into());
    let p = Path::new(&home).join("SenseSim").join("simready-assets");
    let _ = std::fs::create_dir_all(&p);
    p.to_string_lossy().replace('\\', "/")
}

fn save_root(root: &str) {
    if let Some(p) = config_path() {
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, serde_json::json!({ "root": root }).to_string());
    }
}

impl Library {
    pub fn new(root: String) -> Self {
        let (thumbs_tx, thumbs_rx) = unbounded();
        let (online_tx, online_rx) = unbounded::<String>();
        // A few workers fetch online thumbnails, caching them on disk.
        for _ in 0..THUMB_WORKERS {
            let (rx, tx) = (online_rx.clone(), thumbs_tx.clone());
            thread::spawn(move || {
                let dir = cache_dir();
                let _ = std::fs::create_dir_all(&dir);
                for folder in rx {
                    let cached = dir.join(format!("{folder}.png"));
                    let missing = dir.join(format!("{folder}.none"));
                    let img = if cached.exists() {
                        decode_thumb_file(&cached)
                    } else if missing.exists() {
                        None
                    } else {
                        match remote::fetch_thumb(&folder) {
                            Some(bytes) => {
                                let _ = std::fs::write(&cached, &bytes);
                                decode_thumb_bytes(&bytes)
                            }
                            None => {
                                let _ = std::fs::write(&missing, b"");
                                None
                            }
                        }
                    };
                    if tx.send((ThumbKey::Online(folder), img)).is_err() {
                        break;
                    }
                }
            });
        }
        let mut lib = Self {
            root,
            entries: Vec::new(),
            search: String::new(),
            show_remote: true,
            remote_status: String::new(),
            remote: Vec::new(),
            remote_rx: None,
            entry_thumb_keys: Vec::new(),
            thumbs: HashMap::new(),
            thumbs_requested: HashSet::new(),
            thumbs_tx,
            thumbs_rx,
            online_thumb_queue: online_tx,
            downloads: HashMap::new(),
            download_queue: VecDeque::new(),
            last_scan: Instant::now(),
            movable: HashMap::new(),
        };
        lib.refresh_remote();
        lib.rescan();
        lib
    }

    /// Switches to another dataset folder and remembers it for next launch.
    pub fn set_root(&mut self, root: String) {
        self.root = root;
        save_root(&self.root);
        self.rescan();
    }

    /// Fetches the upstream folder list in the background.
    pub fn refresh_remote(&mut self) {
        let (tx, rx) = unbounded();
        thread::spawn(move || {
            let _ = tx.send(remote::list_assets().map_err(|e| format!("{e:#}")));
        });
        self.remote_rx = Some(rx);
        self.remote_status = "fetching catalog…".into();
    }

    /// Re-reads the local folder and merges in upstream-only folders. Cheap
    /// enough to run periodically; loaded thumbnails are kept.
    pub fn rescan(&mut self) {
        self.last_scan = Instant::now();
        let scanned = scan(Path::new(&self.root));
        let local: HashSet<String> = scanned.iter().map(|s| s.folder.clone()).collect();
        let mut list: Vec<(LibraryEntry, Option<ThumbKey>)> = scanned
            .into_iter()
            .map(|s| {
                let key = s.thumb_path.map(ThumbKey::File).or_else(|| Some(ThumbKey::Online(s.folder.clone())));
                (LibraryEntry { name: s.name, folder: s.folder, usd: s.usd, thumb: None, movable: false }, key)
            })
            .collect();
        for folder in &self.remote {
            if !local.contains(folder) {
                list.push((
                    LibraryEntry { name: folder.clone(), folder: folder.clone(), usd: None, thumb: None, movable: false },
                    Some(ThumbKey::Online(folder.clone())),
                ));
            }
        }
        // Assets the assistant made are library assets like any other.
        for (name, usd) in crate::agent::assetgen::list_custom() {
            if !list.iter().any(|(e, _)| e.folder == name) {
                list.push((LibraryEntry { name: name.clone(), folder: name, usd: Some(usd), thumb: None, movable: false }, None));
            }
        }
        list.sort_by_key(|(e, _)| e.name.to_ascii_lowercase());
        self.entries = Vec::with_capacity(list.len());
        self.entry_thumb_keys = Vec::with_capacity(list.len());
        for (mut e, key) in list {
            if let Some(usd) = &e.usd {
                e.movable = *self.movable.entry(usd.clone()).or_insert_with(|| has_joints(usd));
            }
            if let Some(k) = &key {
                e.thumb = self.thumbs.get(k).cloned().flatten();
                // Local files are cheap: load them right away.
                if let ThumbKey::File(p) = k
                    && self.thumbs_requested.insert(k.clone())
                {
                    let (tx, p, k) = (self.thumbs_tx.clone(), p.clone(), k.clone());
                    rayon::spawn(move || {
                        let _ = tx.send((k, decode_thumb_file(&p)));
                    });
                }
            }
            self.entries.push(e);
            self.entry_thumb_keys.push(key);
        }
    }

    /// Called for tiles that are on screen: queues an online thumbnail fetch.
    pub fn want_thumb(&mut self, index: usize) {
        if let Some(Some(ThumbKey::Online(folder))) = self.entry_thumb_keys.get(index) {
            let key = ThumbKey::Online(folder.clone());
            if self.thumbs_requested.insert(key) {
                let _ = self.online_thumb_queue.send(folder.clone());
            }
        }
    }

    pub fn download_state(&self, folder: &str) -> Option<DownloadState> {
        let d = self.downloads.get(folder)?;
        Some(match &d.progress {
            None => DownloadState::Queued,
            Some(p) => DownloadState::Active {
                done: p.done.load(Ordering::Relaxed),
                total: p.total.load(Ordering::Relaxed),
            },
        })
    }

    pub fn download_count(&self) -> (usize, usize) {
        let active = self.downloads.values().filter(|d| d.progress.is_some()).count();
        (active, self.downloads.len() - active)
    }

    pub fn start_download(&mut self, folder: &str, place_when_done: bool) {
        if let Some(d) = self.downloads.get_mut(folder) {
            d.place_when_done |= place_when_done;
            // Clicking a queued asset moves it to the front.
            if place_when_done && let Some(pos) = self.download_queue.iter().position(|f| f == folder) {
                let f = self.download_queue.remove(pos).unwrap();
                self.download_queue.push_front(f);
            }
            return;
        }
        self.downloads.insert(folder.to_string(), Download { progress: None, place_when_done });
        if place_when_done {
            self.download_queue.push_front(folder.to_string());
        } else {
            self.download_queue.push_back(folder.to_string());
        }
    }

    /// Downloads in progress, then the queue in order.
    pub fn downloads(&self) -> Vec<(String, DownloadState)> {
        let mut out: Vec<(String, DownloadState)> = self
            .downloads
            .iter()
            .filter(|(_, d)| d.progress.is_some())
            .filter_map(|(f, _)| Some((f.clone(), self.download_state(f)?)))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out.extend(self.download_queue.iter().map(|f| (f.clone(), DownloadState::Queued)));
        out
    }

    /// Removes one queued (not yet started) download.
    pub fn cancel_download(&mut self, folder: &str) {
        if let Some(pos) = self.download_queue.iter().position(|f| f == folder) {
            self.download_queue.remove(pos);
            self.downloads.remove(folder);
        }
    }

    pub fn cancel_queued(&mut self) {
        for f in std::mem::take(&mut self.download_queue) {
            self.downloads.remove(&f);
        }
    }

    fn pump_queue(&mut self) {
        while self.downloads.values().filter(|d| d.progress.is_some()).count() < MAX_ACTIVE_DOWNLOADS {
            let Some(folder) = self.download_queue.pop_front() else { break };
            let Some(d) = self.downloads.get_mut(&folder) else { continue };
            let progress = Arc::new(Progress::default());
            d.progress = Some(progress.clone());
            let root = PathBuf::from(&self.root);
            thread::spawn(move || remote::download_folder(&root, &folder, progress));
        }
    }

    /// Per-frame housekeeping. Returns messages, USD files to start placing,
    /// and whether the folder was rescanned (so stale load failures can retry).
    pub fn update(&mut self, ctx: &egui::Context) -> (Vec<String>, Vec<PathBuf>, bool) {
        let mut messages = Vec::new();
        let mut place = Vec::new();

        let mut new_thumbs = false;
        for (key, img) in self.thumbs_rx.try_iter().take(48) {
            let tex = img.map(|img| {
                let name = match &key {
                    ThumbKey::File(p) => format!("thumb:{}", p.display()),
                    ThumbKey::Online(f) => format!("thumb-online:{f}"),
                };
                ctx.load_texture(name, img, egui::TextureOptions::LINEAR)
            });
            self.thumbs.insert(key, tex);
            new_thumbs = true;
        }
        if new_thumbs {
            for (e, k) in self.entries.iter_mut().zip(&self.entry_thumb_keys) {
                if e.thumb.is_none()
                    && let Some(k) = k
                {
                    e.thumb = self.thumbs.get(k).cloned().flatten();
                }
            }
        }

        let mut rescan = self.last_scan.elapsed() > AUTO_RESCAN;
        if let Some(rx) = &self.remote_rx
            && let Ok(r) = rx.try_recv()
        {
            match r {
                Ok(list) => {
                    self.remote_status = format!("{} in catalog", list.len());
                    self.remote = list;
                }
                Err(e) => self.remote_status = format!("catalog unavailable: {e}"),
            }
            self.remote_rx = None;
            rescan = true;
        }

        let finished: Vec<String> = self
            .downloads
            .iter()
            .filter(|(_, d)| d.progress.as_ref().is_some_and(|p| p.finished.load(Ordering::Acquire)))
            .map(|(k, _)| k.clone())
            .collect();
        let mut place_folders = Vec::new();
        for f in finished {
            let d = self.downloads.remove(&f).unwrap();
            let err = d.progress.as_ref().and_then(|p| p.error.lock().unwrap().take());
            match err {
                Some(e) => messages.push(format!("Download of {f} failed: {e}")),
                None => {
                    messages.push(format!("Downloaded {f}"));
                    if d.place_when_done {
                        place_folders.push(f);
                    }
                }
            }
            rescan = true;
        }
        self.pump_queue();
        if rescan {
            self.rescan();
        }
        for f in place_folders {
            if let Some(usd) = self.entries.iter().find(|e| e.folder == f).and_then(|e| e.usd.clone()) {
                place.push(usd);
            }
        }
        (messages, place, rescan)
    }
}

fn to_color_image(img: image::DynamicImage) -> egui::ColorImage {
    let img = img.thumbnail(128, 128).to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw())
}

fn decode_thumb_file(path: &Path) -> Option<egui::ColorImage> {
    image::open(path).ok().map(to_color_image)
}

fn decode_thumb_bytes(bytes: &[u8]) -> Option<egui::ColorImage> {
    image::load_from_memory(bytes).ok().map(to_color_image)
}

pub struct Scanned {
    pub name: String,
    pub folder: String,
    pub usd: Option<PathBuf>,
    pub thumb_path: Option<PathBuf>,
}

/// Every asset folder lists its root USDs in `.metadata/...root_usds.json`.
/// Folders without it fall back to any `simready_usd/*.usd*` file. Folders
/// whose root USD is not on disk yet are reported with `usd: None`.
pub fn scan(root: &Path) -> Vec<Scanned> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else { return out };
    let dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    for dir in dirs {
        let name = dir.file_name().unwrap_or_default().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let mut usds = Vec::new();
        let meta = dir.join(".metadata/com.nvidia.simready.root_usds.json");
        if let Ok(txt) = std::fs::read_to_string(&meta)
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt)
        {
            for e in v["entries"].as_array().into_iter().flatten() {
                if let Some(s) = e.as_str() {
                    usds.push(dir.join(s));
                }
            }
        }
        if usds.is_empty()
            && let Ok(rd) = std::fs::read_dir(dir.join("simready_usd"))
        {
            for f in rd.flatten() {
                let p = f.path();
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                if matches!(ext, "usd" | "usda" | "usdc" | "usdz") {
                    usds.push(p);
                }
            }
        }
        if usds.is_empty() {
            // Download just started: show it as incomplete.
            out.push(Scanned { name: name.clone(), folder: name, usd: None, thumb_path: None });
            continue;
        }
        let multi = usds.len() > 1;
        for usd in usds {
            let file = usd.file_name().unwrap_or_default().to_string_lossy().to_string();
            let stem = usd.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let parent = usd.parent().unwrap_or(&dir);
            let thumb_path = [
                parent.join(".thumbs/256x256").join(format!("{file}.png")),
                parent.join(".thumbs").join(format!("{stem}_thumbnail.png")),
            ]
            .into_iter()
            .find(|p| p.exists());
            out.push(Scanned {
                name: if multi { format!("{name} / {stem}") } else { name.clone() },
                folder: name.clone(),
                usd: usd.exists().then_some(usd),
                thumb_path,
            });
        }
    }
    out
}

/// SimReady assets author their joints in `payloads/Physics/physics.usda` next to the root USD.
fn has_joints(usd: &Path) -> bool {
    let Some(dir) = usd.parent() else { return false };
    std::fs::read(dir.join("payloads/Physics/physics.usda")).is_ok_and(|b| {
        let text = String::from_utf8_lossy(&b);
        text.contains("PhysicsRevoluteJoint") || text.contains("PhysicsPrismaticJoint")
    })
}
