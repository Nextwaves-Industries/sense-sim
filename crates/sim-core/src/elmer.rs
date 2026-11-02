//! The low-frequency electromagnetic backend: Elmer, run in our WSL distro. Meshes come from Gmsh
//! as physical groups, which ElmerGrid numbers into the bodies and boundaries a `.sif` file names.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

use crate::jobs::Cancel;
use crate::jobs::wsl::Distro;
use crate::provision;

/// Physical constants as the solver is given them, so that answers can be compared exactly.
pub const EPSILON_0: f64 = 8.854_187_8128e-12;
pub const MU_0: f64 = 1.256_637_062_12e-6;

/// `Constants` section of a `.sif` file.
pub fn constants() -> String {
    format!("Constants\n  Permittivity Of Vacuum = {EPSILON_0:e}\n  Permeability Of Vacuum = {MU_0:e}\nEnd\n")
}

/// A case in the distro.
pub struct Run {
    pub distro: Distro,
    pub remote: String,
}

/// Meshes `geo` with Gmsh here, sends it and `sif` to the distro, converts the mesh and solves.
/// Elmer reports many failures in its output with a zero exit, so its log is checked.
pub fn run(name: &str, local: &Path, geo: &str, sif: &str, files: &[(&str, &[u8])], dimension: u32, cancel: &Cancel, mut on_line: impl FnMut(&str)) -> Result<Run> {
    let gmsh = crate::tools::gmsh().ok_or_else(|| anyhow!("Gmsh is not installed"))?;
    let _ = std::fs::remove_dir_all(local);
    std::fs::create_dir_all(local).with_context(|| format!("creating {}", local.display()))?;
    std::fs::write(local.join("model.geo"), geo)?;
    for (name, bytes) in files {
        std::fs::write(local.join(name), bytes)?;
    }
    std::fs::write(local.join("case.sif"), sif.replace("\r\n", "\n"))?;
    let mut mesh = std::process::Command::new(&gmsh);
    mesh.current_dir(local).args([&format!("-{dimension}"), "model.geo", "-o", "model.msh", "-format", "msh2", "-v", "2"]);
    crate::jobs::process::run(mesh, cancel, &mut on_line).context("meshing")?;
    let distro = provision::distro();
    let remote = format!("/home/solver/runs/{name}");
    distro.upload(local, &remote).context("sending the case to the solver environment")?;
    let script = format!("set -e\ncd {remote}\necho case.sif > ELMERSOLVER_STARTINFO\nElmerGrid 14 2 model.msh -autoclean -out model > log.ElmerGrid\nElmerSolver case.sif\n");
    let mut fatal: Option<String> = None;
    let outcome = distro.run_supervised(&remote, &[("OMP_NUM_THREADS", "8")], &["/bin/bash", "-c", &script], cancel, |line| {
        if fatal.is_none() && (line.contains("ERROR:: ") || line.contains("Fatal")) {
            fatal = Some(line.trim().to_string());
        }
        on_line(line);
    });
    if let Some(f) = fatal {
        bail!("Elmer: {f}");
    }
    outcome.context("running Elmer")?;
    Ok(Run { distro, remote })
}

impl Run {
    pub fn read(&self, path: &str) -> Result<String> {
        let mut text = String::new();
        self.distro.exec(false, &["/usr/bin/cat", &format!("{}/{path}", self.remote)], &Cancel::default(), |l| {
            text.push_str(l);
            text.push('\n');
        })?;
        Ok(text)
    }

    /// A `SaveScalars` file and its `.names`: name to value.
    pub fn scalars(&self, path: &str) -> Result<Vec<(String, f64)>> {
        let names = self.read(&format!("{path}.names"))?;
        let values = self.read(path)?;
        parse_scalars(&names, &values)
    }

    /// A `SaveLine` file and its `.names`, as rows of named columns.
    pub fn line(&self, path: &str) -> Result<(Vec<String>, Vec<Vec<f64>>)> {
        let names = self.read(&format!("{path}.names"))?;
        let values = self.read(path)?;
        parse_columns(&names, &values)
    }
}

/// Column names from a `.names` file: lines like `   3: res: electric energy`.
fn column_names(names: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in names.lines() {
        let line = line.trim();
        let Some((number, rest)) = line.split_once(':') else { continue };
        if number.trim().parse::<usize>().is_ok() {
            out.push(rest.trim().to_lowercase());
        }
    }
    out
}

pub fn parse_scalars(names: &str, values: &str) -> Result<Vec<(String, f64)>> {
    let columns = column_names(names);
    let last = values.lines().rev().find(|l| !l.trim().is_empty()).ok_or_else(|| anyhow!("the scalars file is empty"))?;
    let row: Vec<f64> = last.split_ascii_whitespace().map(|v| v.parse().map_err(|_| anyhow!("bad number {v:?} in the scalars file"))).collect::<Result<_>>()?;
    if row.len() != columns.len() {
        bail!("{} columns named, {} found", columns.len(), row.len());
    }
    Ok(columns.into_iter().zip(row).collect())
}

pub fn parse_columns(names: &str, values: &str) -> Result<(Vec<String>, Vec<Vec<f64>>)> {
    let columns = column_names(names);
    let mut rows = Vec::new();
    for l in values.lines().filter(|l| !l.trim().is_empty()) {
        rows.push(l.split_ascii_whitespace().map(|v| v.parse().map_err(|_| anyhow!("bad number {v:?} in the line file"))).collect::<Result<Vec<f64>>>()?);
    }
    Ok((columns, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_pair_up_with_their_names() {
        let names = "Elmer version: 26.2\nVariables in columns of matrix: scalars.dat\n   1: res: electric energy\n   2: res: capacitance\n";
        let s = parse_scalars(names, "  1.5E-10  3.0E-10\n").unwrap();
        assert_eq!(s[0], ("res: electric energy".to_string(), 1.5e-10));
        assert_eq!(s[1].1, 3.0e-10);
        assert!(parse_scalars(names, "1.0\n").is_err(), "a wrong column count must not pass");
        assert!(parse_scalars(names, "").is_err());
    }

    #[test]
    fn lines_read_as_rows() {
        let (c, r) = parse_columns("   1: coordinate 1\n   2: a\n", "0.0 1.0\n0.5 2.0\n").unwrap();
        assert_eq!(c, ["coordinate 1", "a"]);
        assert_eq!(r, vec![vec![0.0, 1.0], vec![0.5, 2.0]]);
    }
}
