//! Runs CalculiX on a deck and reads back what it wrote.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::frd::{self, Frd};
use crate::jobs::{self, Cancel};
use crate::results::{Plot, StepKind};

/// What a run leaves.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub frd: Frd,
    /// Everything printed to the `.dat` file, as named series.
    pub totals: Vec<Plot>,
}

/// Solves `deck` in `dir` with `threads` threads. CalculiX reports many fatal problems on its
/// output with a zero exit code, so its output is checked as well.
pub fn solve(exe: &Path, dir: &Path, deck: &str, kind: StepKind, threads: usize, cancel: &Cancel, mut on_line: impl FnMut(&str)) -> Result<Run> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(dir.join("job.inp"), deck).context("writing the deck")?;
    for ext in ["frd", "dat", "cvg", "sta"] {
        let _ = std::fs::remove_file(dir.join(format!("job.{ext}")));
    }
    let mut cmd = Command::new(exe);
    cmd.current_dir(dir).arg("job").env("OMP_NUM_THREADS", threads.max(1).to_string()).env("CCX_NPROC_EQUATION_SOLVER", threads.max(1).to_string()).env("CCX_NPROC_RESULTS", threads.max(1).to_string());
    let mut fatal = None;
    jobs::process::run(cmd, cancel, |line| {
        if fatal.is_none() && line.contains("*ERROR") {
            fatal = Some(line.trim().to_string());
        }
        on_line(line);
    })
    .context("running CalculiX")?;
    if let Some(e) = fatal {
        bail!("CalculiX: {e}");
    }
    let path = dir.join("job.frd");
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let frd = frd::parse(&text, kind).context("reading the results")?;
    if frd.steps.is_empty() {
        bail!("CalculiX finished but wrote no results");
    }
    let dat = std::fs::read_to_string(dir.join("job.dat")).unwrap_or_default();
    Ok(Run { frd, totals: parse_totals(&dat) })
}

/// Reads the `total force (fx,fy,fz) for set X and time T` tables of a `.dat` file, as a series
/// per set and direction (`X.fy`).
pub fn parse_totals(dat: &str) -> Vec<Plot> {
    let mut plots: Vec<Plot> = Vec::new();
    let mut lines = dat.lines();
    while let Some(line) = lines.next() {
        let l = line.trim();
        let Some(rest) = l.strip_prefix("total force (fx,fy,fz) for set ") else { continue };
        let Some((set, time)) = rest.split_once(" and time") else { continue };
        let Ok(time) = time.trim().parse::<f64>() else { continue };
        // The values follow after a blank line.
        let Some(values) = lines.by_ref().find(|l| !l.trim().is_empty()) else { break };
        let vals: Vec<f64> = values.split_ascii_whitespace().filter_map(|v| v.parse().ok()).collect();
        for (axis, v) in ["fx", "fy", "fz"].iter().zip(&vals) {
            let name = format!("{}.{axis}", set.trim());
            match plots.iter_mut().find(|p| p.name == name) {
                Some(p) => p.points.push((time, *v)),
                None => plots.push(Plot { name, unit: "N".into(), points: vec![(time, *v)] }),
            }
        }
    }
    plots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_become_series() {
        let dat = "\n total force (fx,fy,fz) for set FIXED and time  0.5000000E+00\n\n       1.0E+01  -2.0E+01   3.0E+00\n\n total force (fx,fy,fz) for set FIXED and time  0.1000000E+01\n\n       2.0E+01  -4.0E+01   6.0E+00\n";
        let p = parse_totals(dat);
        assert_eq!(p.len(), 3);
        assert_eq!(p[1].name, "FIXED.fy");
        assert_eq!(p[1].points, vec![(0.5, -20.0), (1.0, -40.0)]);
    }
}
