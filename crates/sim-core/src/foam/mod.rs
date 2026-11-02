//! The flow backend: OpenFOAM, run in our WSL distro as a separate process. Cases are written as
//! plain text on the Windows side, sent in through a tar pipe, run on the distro's own disk, and
//! their results brought back the same way.

pub mod cases;
pub mod cht;
pub mod mesh_audit;
pub mod study;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::jobs::Cancel;
use crate::jobs::wsl::Distro;
use crate::provision;
use crate::results_vtk::{self, Grid};

/// An OpenFOAM case: file paths inside it, and their text.
#[derive(Debug, Clone, Default)]
pub struct Case {
    pub name: String,
    pub files: BTreeMap<String, String>,
}

impl Case {
    pub fn new(name: &str) -> Self {
        Self { name: name.into(), files: BTreeMap::new() }
    }

    /// Adds a dictionary at `location/object` with its header.
    pub fn dict(&mut self, location: &str, object: &str, class: &str, body: &str) {
        let text = format!("FoamFile\n{{\n    version 2.0;\n    format ascii;\n    class {class};\n    location \"{location}\";\n    object {object};\n}}\n{body}\n");
        self.files.insert(format!("{location}/{object}"), text);
    }

    /// Writes the case into a folder on this side, with Unix line endings.
    pub fn write_to(&self, dir: &Path) -> Result<()> {
        let _ = std::fs::remove_dir_all(dir);
        for (rel, text) in &self.files {
            let path = dir.join(rel);
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p).with_context(|| format!("creating {}", p.display()))?;
            }
            std::fs::write(&path, text.replace("\r\n", "\n")).with_context(|| format!("writing {}", path.display()))?;
        }
        Ok(())
    }
}

/// A case running in the distro.
pub struct Run {
    pub distro: Distro,
    /// Its folder in the distro.
    pub remote: String,
    /// Its folder on this side.
    pub local: PathBuf,
}

/// Keeps the first thing in a solver's output that means the run has failed. OpenFOAM's fatal errors
/// don't always end in a non-zero exit, and running into the memory limit reads differently again.
pub(crate) fn note_fatal(line: &str, fatal: &mut Option<String>) {
    if fatal.is_some() {
        return;
    }
    if line.contains("bad_alloc") || line.contains("Cannot allocate memory") || line.contains("Out of memory") {
        *fatal = Some("It ran out of memory, at the limit that protects your own Linux distros. Use a larger element size.".to_string());
    } else if line.contains("FOAM FATAL") {
        *fatal = Some(line.trim().to_string());
    }
}

/// About how many cells snappyHexMesh makes, refining twice at the surface: a background of `background`
/// cells of size `cell`, and bands of finer cells along `area` of surface.
pub(crate) fn estimated_cells(background: usize, area: f64, cell: f64) -> f64 {
    background as f64 + 64.0 * area / (cell * cell)
}

/// About how much memory a parallel run of that many cells takes at its peak, in bytes.
pub(crate) fn memory_needed(cells: f64) -> f64 {
    8.0 * BASE_PER_RANK + cells * PER_CELL
}

/// What each of a run's 8 processes holds with no cells to speak of, in bytes. Measured on small runs: 8
/// ranks of `rhoCentralFoam` and `chtMultiRegionSimpleFoam` peaked at 0.68 and 0.69 GiB on 40 to 54 thousand
/// cells.
const BASE_PER_RANK: f64 = 100e6;
/// Bytes per cell, across all the processes, at a run's peak. On 1.75 million cells `simpleFoam` peaked at
/// 2.86 GiB and `snappyHexMesh` at 2.15 GiB, so this puts that run about 10% above what it really took. And
/// the cell estimate itself came to 1.74 million there, and 236 thousand on a mesh that had 236 thousand.
const PER_CELL: f64 = 1500.0;

/// Stops a run the VM has no room for, before it can crowd out the user's own distros.
pub(crate) fn check_memory(distro: &Distro, cells: f64) -> Result<()> {
    // Not knowing isn't a reason to refuse: the run's own limit still applies.
    if let Ok(available) = distro.memory_available()
        && let Some(problem) = crate::jobs::wsl::memory_problem(memory_needed(cells), available)
    {
        bail!(problem);
    }
    Ok(())
}

/// Sends `case` to the distro and runs `steps` in turn, each one's output going to `on_line`, ending
/// at the first failure, including an error OpenFOAM doesn't end with a non-zero exit for.
pub fn run(case: &Case, local: &Path, steps: &[&str], cancel: &Cancel, mut on_line: impl FnMut(&str)) -> Result<Run> {
    let distro = provision::distro();
    case.write_to(local)?;
    let remote = format!("/home/solver/runs/{}", case.name);
    distro.upload(local, &remote).context("sending the case to the solver environment")?;
    let script = format!("{}\nset -e\ncd {remote}\n{}\n", provision::foam_env(), steps.join("\n"));
    let mut fatal: Option<String> = None;
    let outcome = distro.run_supervised(&remote, &[], &["/bin/bash", "-c", &script], cancel, |line| {
        note_fatal(line, &mut fatal);
        on_line(line);
    });
    if let Some(f) = fatal {
        bail!("OpenFOAM: {f}");
    }
    outcome.context("running OpenFOAM")?;
    Ok(Run { distro, remote, local: local.to_path_buf() })
}

impl Run {
    /// Converts the latest time to VTK and reads it. Returns the time and its grid. `-poly-decomp`:
    /// snappyHexMesh's refinement leaves polyhedral cells, which would otherwise be holes in what's drawn.
    pub fn latest(&self, region: Option<&str>) -> Result<(f64, Grid)> {
        let region_arg = region.map_or(String::new(), |r| format!("-region {r}"));
        let script = format!("{}\nset -e\ncd {}\nrm -rf VTK\nfoamToVTK -latestTime -legacy -ascii -no-boundary -poly-decomp {region_arg} >/dev/null\n", provision::foam_env(), self.remote);
        self.distro.script(false, &script, &Cancel::default(), |_| {}).context("converting the results")?;
        let out = self.local.join("results");
        let _ = std::fs::remove_dir_all(&out);
        self.distro.download(&self.remote, &out, &["VTK"]).context("fetching the results")?;
        let vtk = find_internal(&out.join("VTK"), region).ok_or_else(|| anyhow!("foamToVTK wrote no grid"))?;
        let text = std::fs::read_to_string(&vtk).with_context(|| format!("reading {}", vtk.display()))?;
        let grid = results_vtk::parse(&text).with_context(|| format!("reading {}", vtk.display()))?;
        let time = grid.time.unwrap_or_else(|| time_of(&vtk));
        Ok((time, grid))
    }

    /// A text file of the case, from the distro.
    pub fn read_remote(&self, path: &str) -> Result<String> {
        let mut text = String::new();
        self.distro.exec(false, &["/usr/bin/cat", &format!("{}/{path}", self.remote)], &Cancel::default(), |l| {
            text.push_str(l);
            text.push('\n');
        })?;
        Ok(text)
    }
}

/// The internal-mesh `.vtk` under foamToVTK's output, in any of its layouts.
fn find_internal(dir: &Path, region: Option<&str>) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    let mut found = Vec::new();
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).ok()?.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "vtk") {
                let text = p.to_string_lossy().replace('\\', "/");
                if !text.contains("/boundary/") && region.is_none_or(|r| text.contains(r)) {
                    found.push(p);
                }
            }
        }
    }
    found.sort();
    found.pop()
}

/// The time step in a foamToVTK file name, for a file with no `TimeValue` in it.
fn time_of(path: &Path) -> f64 {
    path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.rsplit('_').next()).and_then(|s| s.parse().ok()).unwrap_or(0.0)
}

/// Reads residual lines from a solver's output: `Solving for Ux, Initial residual = 0.5, ...`.
pub fn parse_residual(line: &str) -> Option<(&str, f64)> {
    let rest = line.split("Solving for ").nth(1)?;
    let (name, rest) = rest.split_once(',')?;
    let value = rest.split("Initial residual = ").nth(1)?.split(',').next()?.trim().parse().ok()?;
    Some((name.trim(), value))
}

/// The time a solver has reached: `Time = 0.25`.
pub fn parse_time(line: &str) -> Option<f64> {
    let rest = line.strip_prefix("Time = ")?;
    rest.trim().trim_end_matches('s').parse().ok()
}

/// What a steady solver's output says about how far it got: the residual of the flow along x, by
/// iteration, and whether it reached its target or just stopped at the iteration limit. A flow that
/// hasn't settled looks like one that has, so this goes out with the result.
#[derive(Default)]
pub(crate) struct Convergence {
    iteration: f64,
    points: Vec<(f64, f64)>,
    settled: bool,
}

impl Convergence {
    pub(crate) fn line(&mut self, line: &str) {
        if let Some(t) = parse_time(line) {
            self.iteration = t;
        } else if line.contains("solution converged") {
            self.settled = true;
        } else if let Some(rest) = line.split("Solving for Ux, Initial residual = ").nth(1)
            && let Some(value) = rest.split(',').next().and_then(|v| v.trim().parse().ok())
        {
            self.points.push((self.iteration, value));
        }
    }

    pub(crate) fn plot(self) -> crate::results::Plot {
        let name = if self.settled { "Residual along x" } else { "Residual along x, which didn't reach its target by the iteration limit, so this flow may not have settled" };
        crate::results::Plot { name: name.to_string(), unit: String::new(), points: self.points }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_says_whether_it_settled() {
        let log = ["Time = 1", "smoothSolver:  Solving for Ux, Initial residual = 0.5, Final residual = 0.01, No Iterations 2", "smoothSolver:  Solving for Uy, Initial residual = 0.9, Final residual = 0.01, No Iterations 2", "Time = 2", "smoothSolver:  Solving for Ux, Initial residual = 1e-3, Final residual = 1e-5, No Iterations 2"];
        let mut c = Convergence::default();
        log.iter().for_each(|l| c.line(l));
        let plot = c.plot();
        assert_eq!(plot.points, [(1.0, 0.5), (2.0, 1e-3)]);
        assert!(plot.name.contains("may not have settled"), "{}", plot.name);
        let mut c = Convergence::default();
        c.line("SIMPLE solution converged in 157 iterations");
        assert_eq!(c.plot().name, "Residual along x");
    }

    #[test]
    fn running_out_of_memory_is_said_in_words() {
        let mut fatal = None;
        note_fatal("Time = 3", &mut fatal);
        assert!(fatal.is_none());
        note_fatal("terminate called after throwing an instance of 'std::bad_alloc'", &mut fatal);
        assert!(fatal.as_deref().is_some_and(|f| f.contains("ran out of memory")), "{fatal:?}");
        // The first cause is the one to keep.
        note_fatal("--> FOAM FATAL ERROR: something after it", &mut fatal);
        assert!(fatal.as_deref().is_some_and(|f| f.contains("ran out of memory")), "{fatal:?}");
        let mut fatal = None;
        note_fatal("  --> FOAM FATAL ERROR: cannot find file", &mut fatal);
        assert_eq!(fatal.as_deref(), Some("--> FOAM FATAL ERROR: cannot find file"));
    }

    #[test]
    fn progress_is_read_from_solver_output() {
        let line = "smoothSolver:  Solving for Ux, Initial residual = 0.0123, Final residual = 0.001, No Iterations 3";
        assert_eq!(parse_residual(line), Some(("Ux", 0.0123)));
        assert_eq!(parse_time("Time = 250"), Some(250.0));
        assert_eq!(parse_time("Time = 0.5s"), Some(0.5));
        assert_eq!(parse_time("ExecutionTime = 3 s"), None);
    }

    #[test]
    fn a_case_is_written_with_unix_line_endings() {
        let dir = std::env::temp_dir().join("sense-sim-case-test");
        let mut c = Case::new("t");
        c.dict("system", "controlDict", "dictionary", "application simpleFoam;\r\nendTime 10;");
        c.write_to(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join("system/controlDict")).unwrap();
        assert!(!text.contains('\r'));
        assert!(text.contains("object controlDict;"));
    }
}
