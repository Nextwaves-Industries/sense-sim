//! Runs Gmsh as a separate process. Gmsh is GPL and says integrating it into closed-source
//! software needs a commercial licence, so it is never linked: only its executable is run.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::msh::{self, TetMesh};
use crate::jobs::{self, Cancel};

/// Mesh controls common to all FEA runs.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Largest element edge, in the geometry's units.
    pub size: f64,
    pub threads: usize,
}

/// The Gmsh settings every mesh here uses: second-order tetrahedra with curved edges following
/// the geometry, in the only `.msh` flavour the reader takes.
pub fn settings(o: &Options) -> String {
    format!(
        "Mesh.CharacteristicLengthMax = {};\nMesh.ElementOrder = 2;\nMesh.SecondOrderLinear = 0;\nMesh.Optimize = 1;\nMesh.OptimizeNetgen = 1;\nMesh.HighOrderOptimize = 1;\nMesh.MshFileVersion = 4.1;\nMesh.Binary = 0;\nMesh.SaveAll = 1;\nGeneral.NumThreads = {};\n",
        o.size,
        o.threads.max(1)
    )
}

/// Meshes the `.geo` script `geo` in `dir`, returning the mesh. `on_line` sees Gmsh's output.
pub fn mesh_geo(exe: &Path, dir: &Path, geo: &str, cancel: &Cancel, on_line: impl FnMut(&str)) -> Result<TetMesh> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let script = dir.join("model.geo");
    std::fs::write(&script, geo).with_context(|| format!("writing {}", script.display()))?;
    let out = dir.join("model.msh");
    let _ = std::fs::remove_file(&out);
    let mut cmd = Command::new(exe);
    cmd.current_dir(dir).args(["-3", "model.geo", "-o", "model.msh", "-format", "msh41", "-v", "4"]);
    jobs::process::run(cmd, cancel, on_line).context("meshing")?;
    if !out.exists() {
        bail!("Gmsh finished but wrote no mesh");
    }
    let text = std::fs::read_to_string(&out).context("reading the mesh")?;
    let mesh = msh::parse(&text).context("reading the mesh")?;
    if mesh.tets.is_empty() {
        bail!("Gmsh produced no tetrahedra: is the geometry a closed solid?");
    }
    let bad = mesh.inverted_tets();
    if bad > 0 {
        bail!("{bad} of {} elements are inverted where they follow curved geometry. A smaller element size usually cures this.", mesh.tets.len());
    }
    Ok(mesh)
}
