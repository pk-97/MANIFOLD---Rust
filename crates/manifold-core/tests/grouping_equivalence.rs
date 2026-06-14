//! THROWAWAY grouping-equivalence gate (docs/GROUPING_GRAPHS.md §8).
//!
//! For every shipping preset, flatten the on-disk version and its pre-grouping
//! baseline (snapshot in `/tmp/grouping-baselines/`) and assert they are
//! byte-identical *in nodeId space* — the only space that survives the id
//! renumbering and handle-prefixing the flattener does. Three sets must match:
//!   1. connectivity   {(fromNodeId, fromPort, toNodeId, toPort)}
//!   2. per-node facts  {(nodeId, typeId, params, wgslSource)}
//!   3. binding targets every preset binding's target nodeId still exists
//!
//! If all three match for a preset, grouping it changed nothing the runtime
//! computes. Delete this file after the grouping campaign (it depends on a
//! scratch baseline dir and cannot run on a clean checkout).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::flatten::flatten_groups;

const BASELINE_DIR: &str = "/tmp/grouping-baselines";

fn assets_dir() -> PathBuf {
    // manifold-core/.. -> crates/ ; crates/manifold-renderer/assets
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../manifold-renderer/assets")
}

fn load(path: &Path) -> EffectGraphDef {
    let json = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// Flatten, returning the error message instead of panicking so one run reports
/// every broken preset (a malformed group only breaks its own preset).
fn flat(def: &EffectGraphDef) -> Result<EffectGraphDef, String> {
    flatten_groups(def).map_err(|e| e.to_string())
}

/// `id -> nodeId` for the flattened doc. Panics on an empty nodeId so a dropped
/// stable id can't silently collapse two nodes into one set entry.
fn id_to_node_id(def: &EffectGraphDef, label: &str) -> HashMap<u32, String> {
    def.nodes
        .iter()
        .map(|n| {
            assert!(
                !n.node_id.is_empty(),
                "{label}: node id {} (type {}) has an empty nodeId after flatten",
                n.id,
                n.type_id
            );
            (n.id, n.node_id.as_str().to_string())
        })
        .collect()
}

/// {(fromNodeId, fromPort, toNodeId, toPort)} for every wire.
fn connectivity(def: &EffectGraphDef, label: &str) -> HashSet<(String, String, String, String)> {
    let map = id_to_node_id(def, label);
    def.wires
        .iter()
        .map(|w| {
            let from = map.get(&w.from_node).cloned().unwrap_or_else(|| {
                panic!("{label}: wire fromNode {} has no node", w.from_node)
            });
            let to = map
                .get(&w.to_node)
                .cloned()
                .unwrap_or_else(|| panic!("{label}: wire toNode {} has no node", w.to_node));
            (from, w.from_port.clone(), to, w.to_port.clone())
        })
        .collect()
}

/// {(nodeId, typeId, params-json, wgslSource)} — catches a dropped param or a
/// mangled shader body.
fn node_facts(def: &EffectGraphDef) -> HashSet<(String, String, String, String)> {
    def.nodes
        .iter()
        .map(|n| {
            let params = serde_json::to_string(&n.params).unwrap();
            let wgsl = n.wgsl_source.clone().unwrap_or_default();
            (
                n.node_id.as_str().to_string(),
                n.type_id.clone(),
                params,
                wgsl,
            )
        })
        .collect()
}

#[test]
fn grouping_is_flatten_equivalent_to_baseline() {
    let baseline_dir = Path::new(BASELINE_DIR);
    assert!(
        baseline_dir.is_dir(),
        "baseline dir {BASELINE_DIR} missing — snapshot pre-grouping presets there first"
    );

    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for sub in ["effect-presets", "generator-presets"] {
        let dir = assets_dir().join(sub);
        for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            let baseline = baseline_dir.join(&name);
            if !baseline.exists() {
                continue; // no baseline -> nothing to compare against
            }
            checked += 1;

            let disk = match flat(&load(&path)) {
                Ok(d) => d,
                Err(e) => {
                    failures.push(format!("{name}: grouped graph fails to flatten: {e}"));
                    continue;
                }
            };
            let base = match flat(&load(&baseline)) {
                Ok(d) => d,
                Err(e) => {
                    failures.push(format!("{name}: baseline fails to flatten: {e}"));
                    continue;
                }
            };

            // 1. connectivity
            let c_disk = connectivity(&disk, &format!("disk {name}"));
            let c_base = connectivity(&base, &format!("baseline {name}"));
            if c_disk != c_base {
                let only_disk: Vec<_> = c_disk.difference(&c_base).take(6).collect();
                let only_base: Vec<_> = c_base.difference(&c_disk).take(6).collect();
                failures.push(format!(
                    "{name}: connectivity differs\n  only in grouped: {only_disk:?}\n  only in baseline: {only_base:?}"
                ));
            }

            // 2. per-node facts
            let f_disk = node_facts(&disk);
            let f_base = node_facts(&base);
            if f_disk != f_base {
                let only_disk: Vec<_> = f_disk
                    .difference(&f_base)
                    .map(|(id, ty, _, _)| format!("{id}:{ty}"))
                    .take(8)
                    .collect();
                let only_base: Vec<_> = f_base
                    .difference(&f_disk)
                    .map(|(id, ty, _, _)| format!("{id}:{ty}"))
                    .take(8)
                    .collect();
                failures.push(format!(
                    "{name}: per-node facts differ\n  only in grouped: {only_disk:?}\n  only in baseline: {only_base:?}"
                ));
            }

            // 3. binding targets resolve into the flattened node set
            let ids: HashSet<&str> = disk.nodes.iter().map(|n| n.node_id.as_str()).collect();
            if let Some(meta) = &disk.preset_metadata {
                for b in &meta.bindings {
                    if let BindingTarget::Node { node_id, param } = &b.target {
                        if !ids.contains(node_id.as_str()) {
                            failures.push(format!(
                                "{name}: binding '{}' targets missing nodeId '{}.{}'",
                                b.id,
                                node_id.as_str(),
                                param
                            ));
                        }
                    }
                }
            }
        }
    }

    assert!(checked > 0, "no presets checked — baseline dir empty?");
    assert!(
        failures.is_empty(),
        "grouping equivalence failed for {} preset(s):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    eprintln!("grouping equivalence OK across {checked} presets");
}
