//! Asset index for the assistant: tokenized names and measured dimensions,
//! so it can match "pallet rack, ~2.5 m tall" to concrete SimReady assets.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::{Value, json};

use crate::library::Library;
use crate::prototype::{ProtoState, PrototypeCache};

#[derive(Resource)]
pub struct Catalog {
    /// USD path (as string) -> bounding box size in meters (x, y, z), Y-up.
    pub dims: HashMap<String, [f32; 3]>,
    pub indexing: usize,
    pub indexed_total: usize,
    tx: Sender<(String, Option<[f32; 3]>)>,
    rx: Receiver<(String, Option<[f32; 3]>)>,
    dirty: bool,
}

fn index_path() -> PathBuf {
    let base = crate::app_paths::data_base();
    base.join("SenseSim").join("asset_dims.json")
}

impl Default for Catalog {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        let dims = std::fs::read_to_string(index_path())
            .ok()
            .and_then(|t| serde_json::from_str::<HashMap<String, [f32; 3]>>(&t).ok())
            .unwrap_or_default();
        Self { dims, indexing: 0, indexed_total: 0, tx, rx, dirty: false }
    }
}

impl Catalog {
    /// Measures every local asset that has no dimensions yet, in the background.
    pub fn index_all(&mut self, library: &Library) {
        let todo: Vec<PathBuf> = library
            .entries
            .iter()
            .filter_map(|e| e.usd.clone())
            .filter(|p| !self.dims.contains_key(&key(p)))
            .collect();
        self.indexing += todo.len();
        self.indexed_total = self.indexing;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
            pool.install(|| {
                use rayon::prelude::*;
                todo.par_iter().for_each(|p| {
                    let d = std::panic::catch_unwind(|| measure(p)).ok().flatten();
                    let _ = tx.send((key(p), d));
                });
            });
        });
    }

    pub fn get(&self, usd: &Path) -> Option<[f32; 3]> {
        self.dims.get(&key(usd)).copied()
    }
}

fn key(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Bounds of an asset in Y-up meters (same convention as the prototype loader).
fn measure(path: &Path) -> Option<[f32; 3]> {
    let loaded = crate::usd_load::load(path).ok()?;
    let rot = if loaded.z_up { Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2) } else { Quat::IDENTITY };
    let to_y_up = Mat4::from_scale_rotation_translation(Vec3::splat(loaded.meters_per_unit), rot, Vec3::ZERO);
    let mut mn = Vec3::splat(f32::MAX);
    let mut mx = Vec3::splat(f32::MIN);
    for part in &loaded.parts {
        let m = to_y_up * Mat4::from_cols_array(&part.world);
        for p in &part.positions {
            let w = m.transform_point3(Vec3::from_array(*p));
            mn = mn.min(w);
            mx = mx.max(w);
        }
    }
    (mn.x <= mx.x).then(|| (mx - mn).to_array())
}

/// Collects background measurements and dims of every loaded prototype.
pub fn update_catalog(mut catalog: ResMut<Catalog>, cache: Res<PrototypeCache>) {
    let mut got = false;
    for (k, d) in catalog.rx.try_iter().collect::<Vec<_>>() {
        catalog.indexing = catalog.indexing.saturating_sub(1);
        if let Some(d) = d {
            catalog.dims.insert(k, d);
        }
        got = true;
    }
    if cache.is_changed() {
        for (p, s) in &cache.states {
            if let ProtoState::Ready(proto) = s {
                let k = key(p);
                if !catalog.dims.contains_key(&k) {
                    catalog.dims.insert(k, (proto.aabb_max - proto.aabb_min).to_array());
                    got = true;
                }
            }
        }
    }
    if got {
        catalog.dirty = true;
    }
    // Save once a batch settles.
    if catalog.dirty && catalog.indexing == 0 {
        catalog.dirty = false;
        let p = index_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, serde_json::to_string(&catalog.dims).unwrap_or_default());
    }
}

// ---------------------------------------------------------------------------
// Search

/// Splits `barrier_fence_metal_a01` / `bodyPart_doorFront` into lowercase
/// words, dropping variant suffixes such as `a01`.
pub fn tokens(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in name.split(|c: char| c == '_' || c == '-' || c == ' ' || c == '/' || c == '.') {
        let mut cur = String::new();
        let chars: Vec<char> = chunk.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            let boundary = i > 0 && c.is_uppercase() && chars[i - 1].is_lowercase();
            if boundary && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(c.to_ascii_lowercase());
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out.retain(|t| !is_variant(t));
    out
}

fn is_variant(t: &str) -> bool {
    let b = t.as_bytes();
    (b.len() == 3 && b[0].is_ascii_alphabetic() && b[1].is_ascii_digit() && b[2].is_ascii_digit())
        || (!b.is_empty() && b.iter().all(|c| c.is_ascii_digit()))
}

fn singular(t: &str) -> &str {
    if t.len() > 4 && ["xes", "ches", "shes", "sses"].iter().any(|s| t.ends_with(s)) {
        &t[..t.len() - 2] // boxes, benches, glasses
    } else if t.len() > 3 && t.ends_with('s') && !t.ends_with("ss") {
        &t[..t.len() - 1]
    } else {
        t
    }
}

pub fn score(query: &[String], name_tokens: &[String]) -> f32 {
    let mut s = 0.0;
    for q in query {
        let q = singular(q);
        let mut best: f32 = 0.0;
        for t in name_tokens {
            let t = singular(t);
            let v = if t == q {
                3.0
            } else if q.len() >= 3 && (t.starts_with(q) || q.starts_with(t) && t.len() >= 3) {
                2.0
            } else if q.len() >= 4 && t.contains(q) {
                1.0
            } else {
                0.0
            };
            best = best.max(v);
        }
        s += best;
    }
    s
}

pub struct Hit {
    pub index: usize,
    pub score: f32,
}

/// Ranks library entries for a query; `target` (w, h, d in m, any may be 0 =
/// unknown) breaks ties by size similarity.
pub fn search(library: &Library, catalog: &Catalog, query: &str, target: [f32; 3], limit: usize) -> Vec<Hit> {
    let q = tokens(query);
    let mut hits: Vec<Hit> = library
        .entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let s = score(&q, &tokens(&e.name));
            if s <= 0.0 {
                return None;
            }
            let mut s = s / q.len().max(1) as f32;
            if let Some(d) = e.usd.as_ref().and_then(|u| catalog.get(u)) {
                s += 0.3 * size_similarity(d, target);
            }
            if e.usd.is_some() {
                s += 0.05; // prefer assets already on disk
            }
            Some(Hit { index: i, score: s })
        })
        .collect();
    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    hits.truncate(limit);
    hits
}

/// 1 = same size, 0 = very different. Footprint axes are compared
/// orientation-independently.
fn size_similarity(d: [f32; 3], t: [f32; 3]) -> f32 {
    let ratio = |a: f32, b: f32| if a <= 0.0 || b <= 0.0 { None } else { Some((a / b).min(b / a)) };
    let mut v = Vec::new();
    if let Some(r) = ratio(d[1], t[1]) {
        v.push(r);
    }
    let (dmax, dmin) = (d[0].max(d[2]), d[0].min(d[2]));
    let (tmax, tmin) = (t[0].max(t[2]), t[0].min(t[2]));
    if let Some(r) = ratio(dmax, tmax) {
        v.push(r);
    }
    if t[0] > 0.0 && t[2] > 0.0
        && let Some(r) = ratio(dmin, tmin)
    {
        v.push(r);
    }
    if v.is_empty() { 0.0 } else { v.iter().sum::<f32>() / v.len() as f32 }
}

pub fn entry_json(library: &Library, catalog: &Catalog, i: usize) -> Value {
    let e = &library.entries[i];
    let mut v = json!({
        "asset": e.usd.as_ref().map(|u| key(u)).unwrap_or_else(|| e.folder.clone()),
        "name": e.name,
        "folder": e.folder,
        "on_disk": e.usd.is_some(),
    });
    if let Some(d) = e.usd.as_ref().and_then(|u| catalog.get(u)) {
        v["size_m"] = json!([round2(d[0]), round2(d[1]), round2(d[2])]);
    }
    v
}

fn round2(v: f32) -> f64 {
    (v as f64 * 100.0).round() / 100.0
}

/// Distinct asset kinds (names without variant suffixes) with counts.
pub fn categories(library: &Library) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for e in &library.entries {
        let stem: Vec<&str> = e.folder.split('_').filter(|t| !is_variant(&t.to_ascii_lowercase())).collect();
        *m.entry(stem.join("_")).or_insert(0) += 1;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_names() {
        assert_eq!(tokens("barrier_fence_metal_a01"), ["barrier", "fence", "metal"]);
        assert_eq!(tokens("bodyPart_doorFront_b12"), ["body", "part", "door", "front"]);
        assert_eq!(tokens("IV_pole_a03"), ["iv", "pole"]);
    }

    #[test]
    fn scores_matches() {
        let n = tokens("box_cardboard_large_a01");
        assert!(score(&tokens("cardboard boxes"), &n) >= 6.0);
        assert!(score(&tokens("card"), &n) > 0.0);
        assert_eq!(score(&tokens("forklift"), &n), 0.0);
    }

    #[test]
    fn size_similarity_is_orientation_free() {
        assert!((size_similarity([1.0, 2.0, 0.5], [0.5, 2.0, 1.0]) - 1.0).abs() < 1e-6);
        assert!(size_similarity([1.0, 2.0, 0.5], [4.0, 0.2, 4.0]) < 0.3);
        assert_eq!(size_similarity([1.0, 1.0, 1.0], [0.0, 0.0, 0.0]), 0.0);
    }
}
