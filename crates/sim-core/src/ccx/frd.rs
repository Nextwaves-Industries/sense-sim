//! Reader for CalculiX's `.frd` result files (ASCII). Columns are fixed-width and numbers can
//! touch (`-1.0E+00-2.0E+00`), so values are cut by position, never by whitespace.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};

use crate::results::{Field, Location, ResultStep, StepKind};

/// Field names CalculiX uses, with the unit each has in an SI deck.
fn unit_of(name: &str) -> &'static str {
    match name {
        "DISP" | "DISPR" | "DISPI" | "PDISP" => "m",
        "STRESS" | "ZZSTR" | "PE" => "Pa",
        "NDTEMP" => "°C",
        "FLUX" => "W/m²",
        _ => "",
    }
}

/// A frd file: node positions and the result blocks in file order.
#[derive(Debug, Clone, Default)]
pub struct Frd {
    /// Index by the order nodes were first seen. `node_ids[i]` is the solver's id of node `i`.
    pub node_ids: Vec<u32>,
    pub nodes: Vec<[f64; 3]>,
    pub steps: Vec<ResultStep>,
}

/// Cuts `line` into a key (3), an id of `id_width` and floats of width 12.
fn floats(line: &str, id_width: usize) -> Result<(u32, Vec<f32>)> {
    let bytes = line.as_bytes();
    let id_end = 3 + id_width;
    if bytes.len() < id_end {
        bail!("short data line {line:?}");
    }
    let id = line[3..id_end].trim().parse::<u32>().with_context(|| format!("bad id in {line:?}"))?;
    let mut vals = Vec::with_capacity((bytes.len() - id_end) / 12);
    let mut at = id_end;
    while at + 12 <= bytes.len() {
        vals.push(parse_float(&line[at..at + 12]));
        at += 12;
    }
    Ok((id, vals))
}

/// Fortran can drop the `E` from a three-digit exponent (`1.234-100`). Underflow is zero; a value
/// that can't be read is NaN, so that it shows up as missing rather than silently as zero.
fn parse_float(s: &str) -> f32 {
    let t = s.trim();
    if let Ok(v) = t.parse::<f32>() {
        return v;
    }
    // "1.234-100" or "1.234+100".
    if let Some(i) = t.rfind(['+', '-']).filter(|i| *i > 0 && !t[..*i].ends_with(['e', 'E'])) {
        let fixed = format!("{}E{}", &t[..i], &t[i..]);
        if let Ok(v) = fixed.parse::<f32>() {
            return v;
        }
    }
    f32::NAN
}

/// Reads a frd file's text. `kind` says what step values mean in a run of this type; each block's
/// own header decides when CalculiX states it (modes in a mixed run).
pub fn parse(text: &str, kind: StepKind) -> Result<Frd> {
    let mut frd = Frd::default();
    let mut index: HashMap<u32, u32> = HashMap::new();
    let mut lines = text.lines().enumerate().peekable();
    let mut step_value = 0.0f64;
    let mut step_kind = kind;
    let mut pending: Option<ResultStep> = None;
    while let Some((n, raw)) = lines.next() {
        let line = raw.trim_end();
        let head = line.trim_start();
        if head.starts_with("2C") {
            let id_width = id_width_of(line);
            for (m, raw) in lines.by_ref() {
                let l = raw.trim_end();
                if l.starts_with(" -3") {
                    break;
                }
                if !l.starts_with(" -1") {
                    bail!("line {}: expected a node, found {l:?}", m + 1);
                }
                let (id, v) = floats(l, id_width).with_context(|| format!("line {}", m + 1))?;
                if v.len() < 3 {
                    bail!("line {}: a node needs three coordinates", m + 1);
                }
                index.insert(id, frd.nodes.len() as u32);
                frd.node_ids.push(id);
                frd.nodes.push([v[0] as f64, v[1] as f64, v[2] as f64]);
            }
        } else if head.starts_with("3C") {
            // Elements are taken from the solver's mesh, which is known already.
            for (_, raw) in lines.by_ref() {
                if raw.starts_with(" -3") {
                    break;
                }
            }
        } else if head.starts_with("100C") {
            // "  100CL  101 4.17800E+01 ...": the value is at a fixed place after the set number.
            let value = line.get(12..24).map(parse_float).filter(|v| v.is_finite()).ok_or_else(|| anyhow!("line {}: no step value in {line:?}", n + 1))?;
            step_value = value as f64;
            let same = pending.as_ref().is_some_and(|p| p.value.to_bits() == step_value.to_bits());
            // A new value starts a new step; several fields of one step share a value.
            if !same && let Some(p) = pending.take() {
                frd.steps.push(p);
            }
            // CalculiX flags a mode with "MODAL" in the parameter lines.
            step_kind = kind;
        } else if line.starts_with(" -4") {
            let name = line.get(3..).unwrap_or("").split_ascii_whitespace().next().unwrap_or("").to_string();
            let mut labels = Vec::new();
            let mut id_width = 10;
            // Component rows.
            while let Some((_, peek)) = lines.peek() {
                if !peek.starts_with(" -5") {
                    break;
                }
                let (_, row) = lines.next().ok_or_else(|| anyhow!("unexpected end of file"))?;
                let label = row.get(3..).unwrap_or("").split_ascii_whitespace().next().unwrap_or("").to_string();
                // "ALL" is a derived total, listed but not stored.
                if !row.contains("ALL") && !label.is_empty() {
                    labels.push(label);
                }
            }
            let stride = labels.len().max(1);
            let mut data = vec![f32::NAN; frd.nodes.len() * stride];
            let mut current: Option<(u32, usize)> = None;
            for (m, raw) in lines.by_ref() {
                let l = raw.trim_end();
                if l.starts_with(" -3") {
                    break;
                }
                if l.starts_with(" -1") {
                    id_width = 10;
                    let (id, v) = floats(l, id_width).with_context(|| format!("line {}", m + 1))?;
                    let at = *index.get(&id).ok_or_else(|| anyhow!("line {}: result for unknown node {id}", m + 1))? as usize;
                    let take = v.len().min(stride);
                    data[at * stride..at * stride + take].copy_from_slice(&v[..take]);
                    current = Some((id, take));
                } else if l.starts_with(" -2") {
                    // A row continuing the previous node, for more than six components.
                    let Some((id, filled)) = current else { bail!("line {}: continuation with no node", m + 1) };
                    let cont = floats(&format!("{}{:>10}{}", " -1", id, l.get(13..).unwrap_or("")), id_width)?;
                    let at = index[&id] as usize;
                    let take = cont.1.len().min(stride - filled.min(stride));
                    data[at * stride + filled..at * stride + filled + take].copy_from_slice(&cont.1[..take]);
                    current = Some((id, filled + take));
                }
            }
            let step = pending.get_or_insert_with(|| ResultStep { kind: step_kind, value: step_value, fields: Vec::new() });
            let unit = unit_of(&name).to_string();
            // Complex output of harmonic runs arrives as two blocks of one name; keep them apart. In any
            // other run a repeat is a later increment at the same time, and the later one is the answer:
            // contact runs end with a tiny extra increment that does this.
            let field = Field { name: name.clone(), unit, location: Location::Node, components: labels, data: Arc::from(data) };
            match step.fields.iter_mut().find(|f| f.name == name) {
                Some(earlier) if step_kind != StepKind::Frequency => *earlier = field,
                Some(_) => step.fields.push(Field { name: format!("{name}_IM"), ..field }),
                None => step.fields.push(field),
            }
        }
    }
    if let Some(p) = pending {
        frd.steps.push(p);
    }
    Ok(frd)
}

/// The id width of a block: `    2C        1234                                     1` ends in the
/// format flag, 1 for wide ids and 0 for the old five-digit ones.
fn id_width_of(line: &str) -> usize {
    match line.split_ascii_whitespace().last() {
        Some("0") => 5,
        _ => 10,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        "    1C\n",
        "    2C                     2                                     1\n",
        " -1         1 0.00000E+00 0.00000E+00 0.00000E+00\n",
        " -1         2 1.00000E+00-2.50000E-01 5.00000E-01\n",
        " -3\n",
        "    3C                     0                                     1\n",
        " -3\n",
        "    1PSTEP                 1           1           1\n",
        "  100CL  101 4.17800E+01           2                     0    1           1\n",
        " -4  DISP        4    1\n",
        " -5  D1          1    2    1    0\n",
        " -5  D2          1    2    2    0\n",
        " -5  D3          1    2    3    0\n",
        " -5  ALL         1    2    0    0    1ALL\n",
        " -1         1 1.00000E-03-2.00000E-03 3.00000E-03\n",
        " -1         2 4.00000E-03-5.00000E-03-6.00000E-03\n",
        " -3\n",
        "  100CL  102 2.62000E+02           2                     0    2           1\n",
        " -4  NDTEMP      1    1\n",
        " -5  T           1    1    0    0\n",
        " -1         1 3.00000E+02\n",
        " -1         2 3.50000E+02\n",
        " -3\n",
        " 9999\n",
    );

    #[test]
    fn reads_touching_columns_and_steps() {
        let f = parse(SAMPLE, StepKind::Mode).unwrap();
        assert_eq!(f.nodes, vec![[0.0, 0.0, 0.0], [1.0, -0.25, 0.5]]);
        assert_eq!(f.steps.len(), 2);
        assert!((f.steps[0].value - 41.78).abs() < 1e-3);
        let d = f.steps[0].field("DISP").unwrap();
        assert_eq!(d.components, vec!["D1", "D2", "D3"]);
        assert_eq!(d.at(1), &[4e-3, -5e-3, -6e-3]);
        assert_eq!(d.unit, "m");
        assert_eq!(f.steps[1].field("NDTEMP").unwrap().at(1), &[350.0]);
    }

    #[test]
    fn fortran_exponent_without_e() {
        assert!((parse_float(" 1.23400-100") - 0.0).abs() < 1e-30);
        assert!((parse_float("-2.50000E-01") + 0.25).abs() < 1e-6);
        assert!(parse_float("  garbage   ").is_nan());
    }

    #[test]
    fn missing_results_are_nan_not_zero() {
        let text = SAMPLE.replace(" -1         2 4.00000E-03-5.00000E-03-6.00000E-03\n", "");
        let f = parse(&text, StepKind::Time).unwrap();
        assert!(f.steps[0].field("DISP").unwrap().at(1)[0].is_nan());
    }

    #[test]
    fn broken_files_are_errors_not_panics() {
        assert!(parse("    2C   2   1\n garbage\n", StepKind::Time).is_err());
        let unknown = SAMPLE.replace(" -1         1 1.00000E-03", " -1        99 1.00000E-03");
        assert!(parse(&unknown, StepKind::Time).unwrap_err().to_string().contains("unknown node"));
    }
}
