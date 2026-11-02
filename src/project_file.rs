//! The `.sesim` project file: one compressed file holding a whole project, the way a `.docx` does.
//!
//! The scene, positioning, plant, simulation and factory data each read this file, add their part
//! and write it back, so all of them go through here. What a file is gets decided from its first
//! bytes rather than its name, so layouts saved as `.json` by earlier versions still open.

use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

pub const EXTENSION: &str = "sesim";

/// Comes first, uncompressed, so a file says what it is to anyone who looks.
const MARK_ENTRY: &str = "sense-sim-project";
const MARK: &str = "Sense Sim project, format 1\n";
const PROJECT_ENTRY: &str = "project";
/// Far past any real project, so a damaged or hostile file can't exhaust memory.
const LARGEST_PROJECT: u64 = 1 << 30;
/// How every zip starts.
const ZIP_MAGIC: &[u8; 4] = b"PK\x03\x04";

pub fn is_project_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
}

/// Where saving goes: an older `.json` layout becomes a `.sesim` beside it, and stays as it was.
pub fn save_path(path: &Path) -> PathBuf {
    if is_project_path(path) { path.to_path_buf() } else { path.with_extension(EXTENSION) }
}

/// Reads a project, or a layout from an earlier version.
pub fn read(path: &Path) -> Result<Value> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.starts_with(ZIP_MAGIC) {
        read_container(&bytes).with_context(|| format!("{} is damaged, or isn't a Sense Sim project", path.display()))
    } else {
        serde_json::from_slice(&bytes).with_context(|| format!("{} isn't a Sense Sim project or layout", path.display()))
    }
}

fn read_container(bytes: &[u8]) -> Result<Value> {
    let mut zip = ZipArchive::new(Cursor::new(bytes))?;
    if zip.by_name(MARK_ENTRY).is_err() {
        bail!("it has no project mark");
    }
    let entry = zip.by_name(PROJECT_ENTRY).map_err(|_| anyhow!("it has no project in it"))?;
    let mut text = Vec::new();
    entry.take(LARGEST_PROJECT).read_to_end(&mut text)?;
    Ok(serde_json::from_slice(&text)?)
}

/// Writes a project: a container for `.sesim`, and plain text for anything else, which only the
/// app's own temporary files and old callers use. A crash midway leaves the old file whole.
pub fn write(path: &Path, doc: &Value) -> Result<()> {
    let bytes = if is_project_path(path) { container(path, doc)? } else { serde_json::to_string_pretty(doc)?.into_bytes() };
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".saving");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

fn container(path: &Path, doc: &Value) -> Result<Vec<u8>> {
    let mut out = ZipWriter::new(Cursor::new(Vec::new()));
    out.start_file(MARK_ENTRY, SimpleFileOptions::default().compression_method(CompressionMethod::Stored))?;
    out.write_all(MARK.as_bytes())?;
    out.start_file(PROJECT_ENTRY, SimpleFileOptions::default().compression_method(CompressionMethod::Deflated))?;
    out.write_all(&serde_json::to_vec(doc)?)?;
    // A part from a newer version of the app isn't this one's to lose.
    if let Ok(old) = std::fs::read(path)
        && old.starts_with(ZIP_MAGIC)
        && let Ok(mut zip) = ZipArchive::new(Cursor::new(old))
    {
        for i in 0..zip.len() {
            let entry = zip.by_index_raw(i)?;
            let name = entry.name()?.to_string();
            if name != MARK_ENTRY && name != PROJECT_ENTRY {
                out.raw_copy_file(entry)?;
            }
        }
    }
    Ok(out.finish()?.into_inner())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn folder(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sense-sim-project-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn doc() -> Value {
        json!({ "format": "sense_sim_layout", "version": 2, "objects": [{ "name": "Kệ hàng 3", "asset": "C:/assets/rack.usd", "translation": [1.5, 0.0, -2.25] }] })
    }

    #[test]
    fn a_project_comes_back_as_it_went_in() {
        let d = folder("round");
        let p = d.join("warehouse.sesim");
        write(&p, &doc()).unwrap();
        assert_eq!(read(&p).unwrap(), doc());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn it_isnt_json_on_disk() {
        let d = folder("binary");
        let p = d.join("warehouse.sesim");
        write(&p, &doc()).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        assert!(serde_json::from_slice::<Value>(&bytes).is_err());
        assert!(bytes.starts_with(ZIP_MAGIC));
        assert!(!bytes.windows(7).any(|w| w == b"objects"), "the contents are compressed");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn layouts_from_earlier_versions_still_open() {
        let d = folder("old");
        let p = d.join("old.json");
        std::fs::write(&p, serde_json::to_string_pretty(&doc()).unwrap()).unwrap();
        assert_eq!(read(&p).unwrap(), doc());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// What a file is comes from its contents, so a renamed one still opens.
    #[test]
    fn the_name_doesnt_decide_what_a_file_is() {
        let d = folder("renamed");
        let a = d.join("a.sesim");
        write(&a, &doc()).unwrap();
        let renamed = d.join("copy of a.bak");
        std::fs::copy(&a, &renamed).unwrap();
        assert_eq!(read(&renamed).unwrap(), doc());
        let plain = d.join("plain.sesim");
        std::fs::write(&plain, doc().to_string()).unwrap();
        assert_eq!(read(&plain).unwrap(), doc());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn saving_turns_a_json_path_into_a_sesim_beside_it() {
        assert_eq!(save_path(Path::new(r"C:\w\site.json")), PathBuf::from(r"C:\w\site.sesim"));
        assert_eq!(save_path(Path::new(r"C:\w\site.sesim")), PathBuf::from(r"C:\w\site.sesim"));
        assert_eq!(save_path(Path::new(r"C:\w\SITE.SESIM")), PathBuf::from(r"C:\w\SITE.SESIM"));
        assert!(is_project_path(Path::new("a.SeSim")) && !is_project_path(Path::new("a.json")));
    }

    /// The scene writes first, then positioning, plant and the rest each add their part.
    #[test]
    fn each_system_can_add_its_part() {
        let d = folder("parts");
        let p = d.join("site.sesim");
        write(&p, &doc()).unwrap();
        for (key, value) in [("rtls", json!({ "devices": 3 })), ("plant", json!({ "links": [] })), ("simulation", json!({ "studies": [] }))] {
            let mut v = read(&p).unwrap();
            v[key] = value;
            write(&p, &v).unwrap();
        }
        let v = read(&p).unwrap();
        assert_eq!(v["rtls"]["devices"], 3);
        assert!(v["plant"].is_object() && v["simulation"].is_object());
        assert_eq!(v["objects"], doc()["objects"], "nothing lost on the way");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_part_from_a_newer_version_survives_a_save() {
        let d = folder("newer");
        let p = d.join("site.sesim");
        let mut z = ZipWriter::new(Cursor::new(Vec::new()));
        z.start_file(MARK_ENTRY, SimpleFileOptions::default()).unwrap();
        z.write_all(MARK.as_bytes()).unwrap();
        z.start_file(PROJECT_ENTRY, SimpleFileOptions::default()).unwrap();
        z.write_all(doc().to_string().as_bytes()).unwrap();
        z.start_file("from-the-future.bin", SimpleFileOptions::default()).unwrap();
        z.write_all(&[1, 2, 3]).unwrap();
        std::fs::write(&p, z.finish().unwrap().into_inner()).unwrap();

        write(&p, &json!({ "objects": [] })).unwrap();
        let mut zip = ZipArchive::new(std::fs::File::open(&p).unwrap()).unwrap();
        let mut kept = Vec::new();
        zip.by_name("from-the-future.bin").unwrap().read_to_end(&mut kept).unwrap();
        assert_eq!(kept, [1, 2, 3]);
        assert_eq!(zip.len(), 3, "no entry twice");
        drop(zip);
        assert_eq!(read(&p).unwrap(), json!({ "objects": [] }));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn saving_leaves_no_stray_files_and_replaces_in_one_step() {
        let d = folder("atomic");
        let p = d.join("site.sesim");
        write(&p, &doc()).unwrap();
        write(&p, &json!({ "second": true })).unwrap();
        let names: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert_eq!(names, ["site.sesim"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_bad_file_is_an_error_and_says_which_one() {
        let d = folder("bad");
        let good = d.join("good.sesim");
        write(&good, &doc()).unwrap();
        let bytes = std::fs::read(&good).unwrap();

        let cases: [(&str, Vec<u8>); 4] = [
            ("truncated.sesim", bytes[..bytes.len() / 2].to_vec()),
            ("noise.sesim", vec![0xFF; 500]),
            ("empty.sesim", Vec::new()),
            ("other-zip.sesim", {
                let mut z = ZipWriter::new(Cursor::new(Vec::new()));
                z.start_file("readme.txt", SimpleFileOptions::default()).unwrap();
                z.write_all(b"not a project").unwrap();
                z.finish().unwrap().into_inner()
            }),
        ];
        for (name, data) in cases {
            let p = d.join(name);
            std::fs::write(&p, data).unwrap();
            let e = format!("{:#}", read(&p).unwrap_err());
            assert!(e.contains(name), "{name}: {e}");
        }
        assert!(read(&d.join("missing.sesim")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
