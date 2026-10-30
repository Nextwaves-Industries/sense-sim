//! Hugging Face dataset access: lists the asset folders available upstream and
//! downloads individual asset folders on demand.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{Context, Result, bail};

pub const REPO: &str = "nvidia/simready-assets";

fn token() -> Option<String> {
    if let Ok(t) = std::env::var("HF_TOKEN")
        && !t.trim().is_empty()
    {
        return Some(t.trim().to_string());
    }
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).ok()?;
    let t = std::fs::read_to_string(Path::new(&home).join(".cache/huggingface/token")).ok()?;
    let t = t.trim().to_string();
    (!t.is_empty()).then_some(t)
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>> {
    let mut req = ureq::get(url).header("User-Agent", "simready_viewer");
    if let Some(t) = token() {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    req.call().with_context(|| format!("GET {url}"))
}

fn next_link(resp: &ureq::http::Response<ureq::Body>) -> Option<String> {
    let link = resp.headers().get("link")?.to_str().ok()?;
    link.split(',').find(|p| p.contains("rel=\"next\"")).and_then(|p| {
        let s = p.find('<')? + 1;
        let e = p.find('>')?;
        Some(p[s..e].to_string())
    })
}

/// Follows the tree API's `Link: rel="next"` pagination.
fn list_tree(url: &str) -> Result<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    let mut next = Some(url.to_string());
    while let Some(u) = next {
        let mut resp = get(&u)?;
        next = next_link(&resp);
        let body = resp.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string()?;
        let v: serde_json::Value = serde_json::from_str(&body)?;
        out.extend(v.as_array().cloned().unwrap_or_default());
    }
    Ok(out)
}

/// Top-level asset folder names in the dataset.
pub fn list_assets() -> Result<Vec<String>> {
    let url = format!("https://huggingface.co/api/datasets/{REPO}/tree/main");
    let mut names: Vec<String> = list_tree(&url)?
        .into_iter()
        .filter(|e| e["type"] == "directory")
        .filter_map(|e| e["path"].as_str().map(str::to_string))
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort_by_key(|n| n.to_ascii_lowercase());
    Ok(names)
}

/// Progress of one folder download, shared with the UI.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub finished: AtomicBool,
    pub error: std::sync::Mutex<Option<String>>,
}

/// Downloads every file of `folder` into `root/folder`, skipping files that
/// already exist with the right size. Files land via `.part` + rename so a
/// half-written file is never mistaken for a complete one.
pub fn download_folder(root: &Path, folder: &str, progress: Arc<Progress>) {
    let r = (|| -> Result<()> {
        let url = format!("https://huggingface.co/api/datasets/{REPO}/tree/main/{folder}?recursive=true");
        let files: Vec<(String, u64)> = list_tree(&url)?
            .into_iter()
            .filter(|e| e["type"] == "file")
            .filter_map(|e| Some((e["path"].as_str()?.to_string(), e["size"].as_u64().unwrap_or(0))))
            .collect();
        if files.is_empty() {
            bail!("no files found for {folder}");
        }
        let mut todo: Vec<(String, u64)> = files
            .into_iter()
            .filter(|(p, size)| std::fs::metadata(root.join(p)).map(|m| m.len() != *size).unwrap_or(true))
            .collect();
        // Previews first so the tile updates quickly; root USDs last so their
        // presence means everything they reference is already on disk.
        todo.sort_by_key(|(p, _)| priority(p));
        progress.total.store(todo.iter().map(|f| f.1).sum(), Ordering::Relaxed);

        use rayon::prelude::*;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build()?;
        let (early, root_files): (Vec<_>, Vec<_>) = todo.into_iter().partition(|(p, _)| priority(p) < 9);
        pool.install(|| early.par_iter().try_for_each(|(path, _)| download_file(root, path, &progress)))?;
        root_files.iter().try_for_each(|(path, _)| download_file(root, path, &progress))
    })();
    if let Err(e) = r {
        *progress.error.lock().unwrap() = Some(format!("{e:#}"));
    }
    progress.finished.store(true, Ordering::Release);
}

fn download_file(root: &Path, path: &str, progress: &Progress) -> Result<()> {
    let dest: PathBuf = root.join(path);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let encoded: String = path.split('/').map(urlencode).collect::<Vec<_>>().join("/");
    let url = format!("https://huggingface.co/datasets/{REPO}/resolve/main/{encoded}");
    let mut resp = get(&url)?;
    let part = dest.with_file_name(format!("{}.part", dest.file_name().unwrap_or_default().to_string_lossy()));
    let mut f = std::fs::File::create(&part)?;
    let mut reader = resp.body_mut().with_config().limit(u64::MAX).reader();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n])?;
        progress.done.fetch_add(n as u64, Ordering::Relaxed);
    }
    drop(f);
    if std::fs::rename(&part, &dest).is_err() {
        // Another downloader (e.g. `hf download`) may have finished it first.
        let _ = std::fs::remove_file(&part);
        if !dest.exists() {
            bail!("could not write {}", dest.display());
        }
    }
    Ok(())
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn priority(path: &str) -> u8 {
    let parts: Vec<&str> = path.split('/').collect();
    let is_root_usd = parts.len() == 3
        && parts[1] == "simready_usd"
        && [".usd", ".usda", ".usdc", ".usdz"].iter().any(|e| parts[2].ends_with(e));
    if is_root_usd {
        9
    } else if path.contains("/.metadata/") || parts.len() == 2 {
        0
    } else if path.contains("/.thumbs/") {
        1
    } else if path.contains("/payloads/") {
        3
    } else {
        2
    }
}

/// Fetches an asset's 256 px catalog thumbnail without listing the folder,
/// using SimReady's naming convention.
pub fn fetch_thumb(folder: &str) -> Option<Vec<u8>> {
    for ext in ["usd", "usda"] {
        let url = format!(
            "https://huggingface.co/datasets/{REPO}/resolve/main/{folder}/simready_usd/.thumbs/256x256/sm_{folder}_01.{ext}.png"
        );
        if let Ok(mut resp) = get(&url) {
            let mut buf = Vec::new();
            if resp.body_mut().with_config().limit(16 << 20).reader().read_to_end(&mut buf).is_ok() && !buf.is_empty() {
                return Some(buf);
            }
        }
    }
    None
}
