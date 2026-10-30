//! Batch-checks the USD loader: `probe <file.usd | dataset_root>`.
#[path = "../src/usd_load.rs"]
mod usd_load;

use std::path::{Path, PathBuf};

fn root_usds(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else { return out };
    let mut dirs: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for d in dirs {
        let meta = d.join(".metadata/com.nvidia.simready.root_usds.json");
        let Ok(txt) = std::fs::read_to_string(&meta) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { continue };
        for e in v["entries"].as_array().into_iter().flatten() {
            if let Some(s) = e.as_str() {
                out.push(d.join(s));
            }
        }
    }
    out
}

fn main() {
    let arg = PathBuf::from(std::env::args().nth(1).expect("path"));
    let files = if arg.is_dir() { root_usds(&arg) } else { vec![arg] };
    let (mut ok, mut fail, mut empty) = (0, 0, 0);
    for f in &files {
        if !f.exists() { continue; }
        let t = std::time::Instant::now();
        match usd_load::load(f) {
            Ok(l) => {
                let tris: usize = l.parts.iter().map(|p| p.indices.len() / 3).sum();
                let tex = l.materials.iter().filter(|m| m.base_color_tex.is_some()).count();
                if l.parts.is_empty() { empty += 1 } else { ok += 1 }
                println!("{:>6.0}ms prims={:<4} parts={:<3} tris={:<8} mats={} tex={} zup={} mpu={} warn={} {}",
                    t.elapsed().as_secs_f64() * 1000.0, l.prim_count, l.parts.len(), tris, l.materials.len(), tex, l.z_up, l.meters_per_unit, l.warnings.len(),
                    f.file_name().unwrap().to_string_lossy());
                for w in l.warnings.iter().take(2) { println!("    ! {w}"); }
                if files.len() == 1 {
                    for j in &l.joints { println!("  JOINT {j:?}"); }
                    for m in &l.materials { println!("  {m:?}"); }
                }
            }
            Err(e) => { fail += 1; println!("FAIL {}: {e:#}", f.display()); }
        }
    }
    println!("ok={ok} empty={empty} fail={fail} of {}", files.len());
}
