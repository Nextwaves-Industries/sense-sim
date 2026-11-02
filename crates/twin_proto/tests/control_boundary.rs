//! The viewer runs an AI assistant and MCP tools with `&mut World`, so "don't call the
//! command code" isn't enough: it must not be linked at all. The same goes for the
//! collector on plant PCs and for discovery. This reads `Cargo.lock`, so a dependency
//! added anywhere along the chain fails here.

use std::collections::{BTreeMap, BTreeSet};

const COMMANDS: &str = "twin_control_proto";

/// Package name to the names it depends on, merged across versions. Merging can only
/// report too much, never too little, which is the safe direction for this check.
fn graph(lock: &str) -> BTreeMap<String, BTreeSet<String>> {
    let lock: toml::Table = lock.parse().expect("Cargo.lock should parse");
    let packages = lock["package"].as_array().expect("Cargo.lock should list packages");
    let mut graph: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for package in packages {
        let name = package["name"].as_str().expect("package name").to_string();
        let deps = package.get("dependencies").and_then(|d| d.as_array()).into_iter().flatten();
        // An entry is "name" or "name version" when several versions are locked.
        let deps = deps.map(|d| d.as_str().expect("dependency name").split(' ').next().unwrap().to_string());
        graph.entry(name).or_default().extend(deps);
    }
    graph
}

fn reachable(graph: &BTreeMap<String, BTreeSet<String>>, from: &str) -> BTreeSet<String> {
    assert!(graph.contains_key(from), "{from} is missing from Cargo.lock, so this check would prove nothing");
    let mut seen = BTreeSet::new();
    let mut todo = vec![from.to_string()];
    while let Some(name) = todo.pop() {
        if seen.insert(name.clone())
            && let Some(deps) = graph.get(&name)
        {
            todo.extend(deps.iter().cloned());
        }
    }
    seen
}

fn workspace_lock() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock");
    std::fs::read_to_string(path).expect("workspace Cargo.lock")
}

#[test]
fn viewer_collector_and_discovery_cannot_reach_command_code() {
    let graph = graph(&workspace_lock());
    for package in ["sense_sim", "collector", "twin_probes", "twin_proto"] {
        assert!(!reachable(&graph, package).contains(COMMANDS), "{package} must not depend on {COMMANDS}, even indirectly");
    }
}

#[test]
fn the_check_is_not_vacuous_because_the_service_and_console_do_reach_it() {
    let graph = graph(&workspace_lock());
    for package in ["twind", "console"] {
        assert!(reachable(&graph, package).contains(COMMANDS), "{package} should depend on {COMMANDS}");
    }
}

#[test]
fn an_indirect_dependency_with_a_version_suffix_is_still_caught() {
    let lock = r#"
        [[package]]
        name = "sense_sim"
        version = "1.0.0"
        dependencies = ["helper 2.0.0"]

        [[package]]
        name = "helper"
        version = "2.0.0"
        dependencies = ["twin_control_proto"]

        [[package]]
        name = "twin_control_proto"
        version = "0.1.0"
    "#;
    assert!(reachable(&graph(lock), "sense_sim").contains(COMMANDS));
}
