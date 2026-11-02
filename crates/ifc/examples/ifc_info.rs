//! Reads an IFC file and prints what came out, with plausibility checks.
//!
//! ```text
//! cargo run -p ifc --release --example ifc_info -- <file.ifc> [--list]
//! ```
//!
//! `--list` also prints one line per element.
//!
//! Exits non-zero when the file cannot be read, nothing loaded, or a check fails.

use ifc::{Building, Element, ElementKind};
use std::collections::BTreeMap;

/// Thinnest and thickest wall treated as physically plausible, in metres.
const WALL_THICKNESS_RANGE: (f64, f64) = (0.02, 1.5);

fn quantiles(mut values: Vec<f64>) -> Option<String> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let at = |q: f64| values[((values.len() - 1) as f64 * q).round() as usize];
    Some(format!(
        "n={:<5} min {:.3}  p10 {:.3}  median {:.3}  p90 {:.3}  max {:.3}",
        values.len(),
        at(0.0),
        at(0.1),
        at(0.5),
        at(0.9),
        at(1.0),
    ))
}

fn of_kind(building: &Building, kind: ElementKind) -> impl Iterator<Item = &Element> {
    building.elements.iter().filter(move |e| e.kind == kind)
}

/// The reason without its `#id 'name': ` prefix, so that entries can be grouped.
fn reason_group(reason: &str) -> &str {
    reason.split_once("': ").map_or(reason, |(_, rest)| rest)
}

fn main() {
    let mut paths = std::env::args_os().skip(1).filter(|arg| arg != "--list");
    let Some(path) = paths.next() else {
        eprintln!("usage: ifc_info <file.ifc> [--list]");
        std::process::exit(2);
    };
    let list_elements = std::env::args_os().any(|arg| arg == "--list");
    let started = std::time::Instant::now();
    let building = match ifc::read_file(std::path::Path::new(&path)) {
        Ok(building) => building,
        Err(error) => {
            eprintln!("error: {error:#}");
            std::process::exit(1);
        }
    };
    println!("file      {}", path.to_string_lossy());
    println!("read in   {:.3} s", started.elapsed().as_secs_f64());
    println!("building  '{}'", building.name);
    println!("unit      {} m", building.unit_to_m);

    println!("\ncounts (exact share):");
    for (kind, count) in building.counts() {
        let exact = of_kind(&building, kind).filter(|e| e.exact).count();
        println!("  {:<9} {count:>6}   exact {:>5.1} %", format!("{kind:?}"), 100.0 * exact as f64 / count as f64);
    }

    let mut failures: Vec<String> = Vec::new();
    match building.bounds() {
        Some((min, max)) => {
            println!(
                "\nbounds    min [{:.2}, {:.2}, {:.2}]  max [{:.2}, {:.2}, {:.2}]",
                min[0], min[1], min[2], max[0], max[1], max[2]
            );
            println!("extent    {:.2} x {:.2} x {:.2} m", max[0] - min[0], max[1] - min[1], max[2] - min[2]);
        }
        None => failures.push("no elements loaded".to_string()),
    }

    println!("\nsizes in metres:");
    let walls: Vec<&Element> = of_kind(&building, ElementKind::Wall).collect();
    let rows: [(&str, Option<String>); 7] = [
        ("wall thickness", quantiles(walls.iter().filter_map(|e| e.thickness_m).collect())),
        ("wall box short side", quantiles(walls.iter().map(|e| e.size[1]).collect())),
        ("wall length", quantiles(walls.iter().map(|e| e.size[0]).collect())),
        ("wall height", quantiles(walls.iter().map(|e| e.size[2]).collect())),
        ("slab thickness", quantiles(of_kind(&building, ElementKind::Slab).filter_map(|e| e.thickness_m).collect())),
        ("door width/height", {
            let doors: Vec<&Element> = of_kind(&building, ElementKind::Door).collect();
            quantiles(doors.iter().map(|e| e.size[0]).collect())
                .map(|w| format!("{w}\n    {:<20} {}", "", quantiles(doors.iter().map(|e| e.size[2]).collect()).unwrap_or_default()))
        }),
        ("window width", quantiles(of_kind(&building, ElementKind::Window).map(|e| e.size[0]).collect())),
    ];
    for (label, row) in rows {
        if let Some(row) = row {
            println!("  {label:<22} {row}");
        }
    }

    let mut storeys: BTreeMap<String, usize> = BTreeMap::new();
    let mut materials: BTreeMap<String, usize> = BTreeMap::new();
    for element in &building.elements {
        *storeys.entry(element.storey.clone().unwrap_or_else(|| "(none)".to_string())).or_default() += 1;
        *materials.entry(element.material.clone().unwrap_or_else(|| "(none)".to_string())).or_default() += 1;
    }
    println!("\nstoreys:");
    for (storey, count) in &storeys {
        println!("  {count:>6}  {storey}");
    }
    let mut by_count: Vec<(String, usize)> = materials.into_iter().collect();
    by_count.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    println!("\nmaterials ({} distinct, top 12):", by_count.len());
    for (material, count) in by_count.iter().take(12) {
        println!("  {count:>6}  {material}");
    }

    // Whole elements lost, as opposed to loaded elements missing a part.
    let whole = |reason: &str| !reason_group(reason).starts_with("partial:");
    let is_element = |ty: &str| ty != "STEP" && ty != "IFCUNITASSIGNMENT";
    let lost = building.skipped.iter().filter(|(ty, reason)| is_element(ty) && whole(reason)).count();
    let partial = building.skipped.iter().filter(|(_, reason)| !whole(reason)).count();
    let total = building.elements.len() + lost;
    println!(
        "\nskipped: {} entries; {lost} whole elements ({:.1} % of {total}), {partial} partial notes",
        building.skipped.len(),
        if total == 0 { 0.0 } else { 100.0 * lost as f64 / total as f64 },
    );
    let mut groups: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (ty, reason) in &building.skipped {
        let group: String = reason_group(reason).chars().take(110).collect();
        *groups.entry((ty.clone(), group)).or_default() += 1;
    }
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    for ((ty, reason), count) in groups.iter().take(25) {
        println!("  {count:>6}  {ty:<26} {reason}");
    }
    if groups.len() > 25 {
        println!("  ... and {} more kinds of reason", groups.len() - 25);
    }

    // Plausibility: a wrong unit or broken placement shows up in these.
    let (thin, thick) = WALL_THICKNESS_RANGE;
    let mut odd_walls: Vec<&&Element> = walls
        .iter()
        .filter(|e| e.thickness_m.is_some_and(|t| !(thin..=thick).contains(&t)))
        .collect();
    odd_walls.sort_by(|a, b| b.thickness_m.partial_cmp(&a.thickness_m).unwrap_or(std::cmp::Ordering::Equal));
    println!("\nchecks:");
    println!("  walls outside {thin}..{thick} m thick: {} of {}", odd_walls.len(), walls.len());
    for wall in odd_walls.iter().take(8) {
        println!(
            "    t={:.3} size=[{:.2}, {:.2}, {:.2}] exact={} '{}'",
            wall.thickness_m.unwrap_or(0.0),
            wall.size[0],
            wall.size[1],
            wall.size[2],
            wall.exact,
            wall.name
        );
    }
    let disagree: Vec<&&Element> = walls
        .iter()
        .filter(|e| e.exact && e.thickness_m.is_some_and(|t| (t - e.size[1]).abs() > 0.02))
        .collect();
    println!("  exact walls whose stated thickness differs from their box by >2 cm: {}", disagree.len());
    for wall in disagree.iter().take(5) {
        println!(
            "    t={:.3} box={:.3} material={:?} '{}'",
            wall.thickness_m.unwrap_or(0.0),
            wall.size[1],
            wall.material,
            wall.name
        );
    }
    let tall = building.elements.iter().filter(|e| e.size.iter().any(|s| *s > 500.0)).count();
    println!("  elements with a side over 500 m: {tall}");
    if tall > 0 {
        failures.push(format!("{tall} elements have a side over 500 m"));
    }
    let broken = building
        .elements
        .iter()
        .filter(|e| (0..3).any(|axis| e.min[axis] > e.max[axis] || !e.center[axis].is_finite()))
        .count();
    if broken > 0 {
        failures.push(format!("{broken} elements have an inverted or non-finite box"));
    }

    if list_elements {
        println!("\nelements (kind, size, yaw in degrees, centre, thickness, exact, storey, name):");
        for e in &building.elements {
            println!(
                "  {:<8} [{:>7.3} {:>7.3} {:>7.3}] {:>7.2} deg  c[{:>8.2} {:>8.2} {:>7.2}] t={:<6} {} {:<10} {}",
                format!("{:?}", e.kind),
                e.size[0],
                e.size[1],
                e.size[2],
                e.yaw_rad.to_degrees(),
                e.center[0],
                e.center[1],
                e.center[2],
                e.thickness_m.map_or("-".to_string(), |t| format!("{t:.3}")),
                if e.exact { "exact " } else { "approx" },
                e.storey.as_deref().unwrap_or("-"),
                e.name,
            );
        }
    }

    if failures.is_empty() {
        println!("\nOK");
    } else {
        for failure in &failures {
            println!("\nFAILED: {failure}");
        }
        std::process::exit(1);
    }
}
