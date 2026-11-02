//! Where the external solvers live once installed. Each tool has its own folder under
//! `%LOCALAPPDATA%\SenseSim\solvers`.

use std::path::{Path, PathBuf};

/// The folder solvers are installed in.
pub fn solvers_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from);
    base.join("SenseSim").join("solvers")
}

fn first_existing(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.is_file()).cloned()
}

fn under(root: &Path, parts: &[&str]) -> PathBuf {
    parts.iter().fold(root.to_path_buf(), |p, s| p.join(s))
}

pub fn gmsh() -> Option<PathBuf> {
    let r = solvers_dir();
    first_existing(&[under(&r, &["gmsh-4.15.2", "gmsh-4.15.2-Windows64", "gmsh.exe"])])
}

pub fn ccx() -> Option<PathBuf> {
    let r = solvers_dir();
    first_existing(&[under(&r, &["calculix-2.23", "calculix_2.23_4win", "ccx_static.exe"])])
}
