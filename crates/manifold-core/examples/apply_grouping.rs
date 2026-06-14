//! THROWAWAY grouping driver for the grouping campaign (docs/GROUPING_GRAPHS.md).
//!
//! Reads a *pristine flat* preset plus an agent-authored grouping spec and emits
//! the grouped preset. Every node body is pulled VERBATIM through the tested
//! `group_edit::group_selection` collapse transform — no shader source or param
//! is ever retyped — so a grouped graph is flatten-equivalent to its baseline by
//! construction. The fan-out agents only READ and return a spec; this runs in the
//! main session, which is why no subagent ever writes a file or raises a prompt.
//!
//! usage:
//!   cargo run -q -p manifold-core --example apply_grouping -- <baseline.json> <spec.json> <out.json>
//!
//! Spec JSON shape (all fields optional except group `name`):
//!   {
//!     "description": "full breakdown walkthrough...",
//!     "titles": { "<nodeId>": "Display Title", ... },
//!     "groups": [
//!       {
//!         "name": "Flow Field",
//!         "parent": null,                       // or a parent group name for nesting
//!         "members": ["grad", "grad_scaled"],   // direct LEAF nodeIds (not in a child group)
//!         "tint": [0.2, 0.4, 0.8, 1.0],         // optional RGBA header accent
//!         "portRenames": {                      // optional, key = "<in|out>:<innerNodeId>:<innerPort>"
//!           "in:grad:in":  { "name": "blurredDensity", "type": "Texture2D" },
//!           "out:field_blur_v:out": { "name": "forceField", "type": "Texture2D" }
//!         }
//!       }
//!     ]
//!   }
//!
//! Delete this example after the campaign.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID,
};
use manifold_core::group_edit::group_selection;
use serde::Deserialize;

/// Accept an explicit JSON `null` as the field's default. Agents emit `null` for
/// empty collections (e.g. `"portRenames": null`), which a plain
/// `#[serde(default)]` (which only covers a *missing* key) would reject.
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Spec {
    /// Preset name, used in batch mode to locate the baseline + output file.
    #[serde(default)]
    preset: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    titles: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "null_default")]
    groups: Vec<GroupSpec>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupSpec {
    name: String,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    members: Vec<String>,
    #[serde(default)]
    tint: Option<[f32; 4]>,
    #[serde(default, deserialize_with = "null_default")]
    port_renames: BTreeMap<String, PortRename>,
}

#[derive(Deserialize)]
struct PortRename {
    name: String,
    #[serde(rename = "type", default)]
    ty: Option<String>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        // Batch: apply many specs. specs.json is a JSON array; each spec's
        // `preset` field selects its baseline (<baselines>/<preset>.json) and its
        // output (whichever of <assets>/{effect,generator}-presets/<preset>.json
        // exists). Lets the whole tier apply in one command.
        Some("--batch") => {
            if args.len() != 5 {
                eprintln!(
                    "usage: apply_grouping --batch <specs.json> <baselines_dir> <assets_root>"
                );
                std::process::exit(2);
            }
            let (specs_path, baselines, assets) = (&args[2], &args[3], &args[4]);
            let specs: Vec<Spec> =
                serde_json::from_str(&fs::read_to_string(specs_path).expect("read specs"))
                    .expect("parse specs array");
            // Apply each spec independently: a bad nodeId in one spec panics, but
            // catch_unwind keeps it from aborting the rest of the tier. Failures
            // are reported by name so they can be fixed and re-run on their own.
            let mut applied = 0usize;
            let mut failed: Vec<String> = Vec::new();
            for spec in &specs {
                let label = spec.preset.clone().unwrap_or_else(|| "<no preset>".into());
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    apply_batch_one(spec, baselines, assets)
                }));
                match res {
                    Ok(()) => applied += 1,
                    Err(_) => failed.push(label),
                }
            }
            eprintln!("batch: {applied} applied, {} failed {failed:?}", failed.len());
        }
        // Single: <baseline.json> <spec.json> <out.json>
        _ => {
            if args.len() != 4 {
                eprintln!("usage: apply_grouping <baseline.json> <spec.json> <out.json>");
                eprintln!(
                    "   or: apply_grouping --batch <specs.json> <baselines_dir> <assets_root>"
                );
                std::process::exit(2);
            }
            let (baseline_path, spec_path, out_path) = (&args[1], &args[2], &args[3]);
            let def: EffectGraphDef = serde_json::from_str(
                &fs::read_to_string(baseline_path).expect("read baseline"),
            )
            .expect("parse baseline");
            let spec: Spec = serde_json::from_str(
                &fs::read_to_string(spec_path).expect("read spec"),
            )
            .expect("parse spec");
            let grouped = group_one(def, &spec);
            fs::write(out_path, serde_json::to_string_pretty(&grouped).unwrap() + "\n")
                .expect("write out");
            eprintln!("wrote {out_path}");
        }
    }
}

/// Load one preset's baseline, group it per `spec`, and write it back to the
/// asset tree. Panics on any problem (bad nodeId, missing file) so the batch
/// loop's catch_unwind can isolate the failure to this one preset.
fn apply_batch_one(spec: &Spec, baselines: &str, assets: &str) {
    let preset = spec.preset.as_deref().expect("batch spec missing `preset`");
    let baseline = format!("{baselines}/{preset}.json");
    let out = ["effect-presets", "generator-presets"]
        .iter()
        .map(|sub| format!("{assets}/{sub}/{preset}.json"))
        .find(|p| std::path::Path::new(p).exists())
        .unwrap_or_else(|| panic!("{preset}: no asset file under {assets}"));
    let def: EffectGraphDef = serde_json::from_str(
        &fs::read_to_string(&baseline).unwrap_or_else(|e| panic!("read {baseline}: {e}")),
    )
    .unwrap_or_else(|e| panic!("parse {baseline}: {e}"));
    let grouped = group_one(def, spec);
    fs::write(&out, serde_json::to_string_pretty(&grouped).unwrap() + "\n").expect("write");
    eprintln!("grouped {preset} ({} groups) -> {out}", spec.groups.len());
}

/// Apply one grouping spec to a flat def: set titles, replace the description,
/// build groups bottom-up via group_selection (verbatim bodies), and clear the
/// sentinel ids. Returns the grouped def.
fn group_one(mut def: EffectGraphDef, spec: &Spec) -> EffectGraphDef {
    // 1. Titles — applied on the flat list; group_selection clones bodies verbatim,
    //    so a title set now survives into whatever group the node ends up in.
    for n in def.nodes.iter_mut() {
        if let Some(t) = spec.titles.get(n.node_id.as_str()) {
            n.title = Some(t.clone());
        }
    }

    // 2. Description (the breakdown walkthrough).
    if let Some(d) = &spec.description {
        def.description = Some(d.clone());
    }

    // 3. Groups, bottom-up: a group is processed only once all of its children
    //    exist as top-level group nodes (so its selection can reference them).
    let mut nodes = std::mem::take(&mut def.nodes);
    let mut wires = std::mem::take(&mut def.wires);
    let mut done: BTreeSet<String> = BTreeSet::new();
    let mut pending: Vec<&GroupSpec> = spec.groups.iter().collect();

    while !pending.is_empty() {
        let idx = pending
            .iter()
            .position(|g| {
                spec.groups
                    .iter()
                    .filter(|c| c.parent.as_deref() == Some(g.name.as_str()))
                    .all(|c| done.contains(&c.name))
            })
            .expect("grouping spec has a parent cycle");
        let g = pending.remove(idx);

        let mut selected: BTreeSet<u32> = BTreeSet::new();
        for m in &g.members {
            let id = nodes
                .iter()
                .find(|n| n.node_id.as_str() == m)
                .unwrap_or_else(|| {
                    panic!("group '{}': member nodeId '{}' not at top level", g.name, m)
                })
                .id;
            selected.insert(id);
        }
        for child in spec
            .groups
            .iter()
            .filter(|c| c.parent.as_deref() == Some(g.name.as_str()))
        {
            let id = nodes
                .iter()
                .find(|n| n.handle.as_deref() == Some(child.name.as_str()) && n.group.is_some())
                .unwrap_or_else(|| panic!("group '{}': child '{}' missing", g.name, child.name))
                .id;
            selected.insert(id);
        }

        let (nn, nw) = group_selection(nodes, wires, &selected, &g.name, (0.0, 0.0))
            .unwrap_or_else(|e| panic!("group '{}': {e:?}", g.name));
        nodes = nn;
        wires = nw;

        // Stabilise the new group node: nodeId == handle (template convention;
        // group nodeIds fold away so this is cosmetic but keeps diffs clean),
        // tint, and port renames.
        let gnode = nodes
            .iter_mut()
            .find(|n| n.handle.as_deref() == Some(g.name.as_str()) && n.group.is_some())
            .expect("just-created group node");
        gnode.node_id = NodeId::new(g.name.clone());
        gnode.editor_pos = None; // template omits it; let the editor auto-layout
        if let (Some(t), Some(gd)) = (g.tint, gnode.group.as_mut()) {
            gd.tint = Some(t);
        }
        if !g.port_renames.is_empty() {
            apply_port_renames(gnode, &mut wires, &g.port_renames);
        }
        done.insert(g.name.clone());
    }

    def.nodes = nodes;
    def.wires = wires;

    // group_selection mints a random nodeId on each group_input/group_output
    // sentinel; the template omits them and they fold away at flatten, so clear
    // them for deterministic, template-matching output.
    strip_sentinel_ids(&mut def.nodes);
    def
}

/// Clear the nodeId on every `group_input`/`group_output` sentinel, recursing
/// into nested group bodies. Sentinels fold away at flatten so their id is never
/// observed; dropping it keeps output deterministic and matches the hand-authored
/// presets, which omit the field entirely.
fn strip_sentinel_ids(nodes: &mut [EffectGraphNode]) {
    for n in nodes.iter_mut() {
        if n.type_id == GROUP_INPUT_TYPE_ID || n.type_id == GROUP_OUTPUT_TYPE_ID {
            n.node_id = NodeId::default();
        }
        if let Some(gd) = n.group.as_mut() {
            strip_sentinel_ids(&mut gd.nodes);
        }
    }
}

/// Rename a group's auto-inferred boundary ports (which default to the inner
/// port name, e.g. "in"/"out") to the spec's human names, updating all three
/// places a port name lives: the interface decl, the body boundary wire, and the
/// parent wire on the group node. Keyed on the inner endpoint so the rename is
/// stable regardless of inferred ordering.
fn apply_port_renames(
    gnode: &mut EffectGraphNode,
    parent_wires: &mut [EffectGraphWire],
    renames: &BTreeMap<String, PortRename>,
) {
    let gid = gnode.id;
    let gd = gnode.group.as_mut().expect("group body");
    let gi = gd.nodes.iter().find(|n| n.type_id == GROUP_INPUT_TYPE_ID).map(|n| n.id);
    let go = gd.nodes.iter().find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID).map(|n| n.id);
    let body_node_id: BTreeMap<u32, String> = gd
        .nodes
        .iter()
        .map(|n| (n.id, n.node_id.as_str().to_string()))
        .collect();

    // (is_input, old_name, new_name, new_type)
    let mut ops: Vec<(bool, String, String, Option<String>)> = Vec::new();
    for w in &gd.wires {
        if Some(w.from_node) == gi {
            let inner = body_node_id.get(&w.to_node).cloned().unwrap_or_default();
            if let Some(r) = renames.get(&format!("in:{inner}:{}", w.to_port)) {
                ops.push((true, w.from_port.clone(), r.name.clone(), r.ty.clone()));
            }
        }
        if Some(w.to_node) == go {
            let inner = body_node_id.get(&w.from_node).cloned().unwrap_or_default();
            if let Some(r) = renames.get(&format!("out:{inner}:{}", w.from_port)) {
                ops.push((false, w.to_port.clone(), r.name.clone(), r.ty.clone()));
            }
        }
    }

    // Deduplicate rename targets so two distinct ports can't collapse to one
    // interface name (the flattener rejects DuplicateInterfaceName). Agents
    // sometimes name two boundary ports identically. The inferred (old) names are
    // already unique per side, so renaming by old name stays unambiguous; we only
    // need each NEW name to stay unique against the other renames and any port we
    // are leaving at its inferred name.
    for side in [true, false] {
        let iface = if side { &gd.interface.inputs } else { &gd.interface.outputs };
        let renamed_olds: BTreeSet<&str> =
            ops.iter().filter(|o| o.0 == side).map(|o| o.1.as_str()).collect();
        let mut used: BTreeSet<String> = iface
            .iter()
            .map(|p| p.name.clone())
            .filter(|n| !renamed_olds.contains(n.as_str()))
            .collect();
        for op in ops.iter_mut().filter(|o| o.0 == side) {
            let base = op.2.clone();
            let mut cand = base.clone();
            let mut k = 2;
            while used.contains(&cand) {
                cand = format!("{base}{k}");
                k += 1;
            }
            used.insert(cand.clone());
            op.2 = cand;
        }
    }

    for (is_input, old, new, ty) in ops {
        if is_input {
            for p in gd.interface.inputs.iter_mut().filter(|p| p.name == old) {
                p.name = new.clone();
                if let Some(t) = &ty {
                    p.port_type = t.clone();
                }
            }
            for w in gd.wires.iter_mut() {
                if Some(w.from_node) == gi && w.from_port == old {
                    w.from_port = new.clone();
                }
            }
            for w in parent_wires.iter_mut() {
                if w.to_node == gid && w.to_port == old {
                    w.to_port = new.clone();
                }
            }
        } else {
            for p in gd.interface.outputs.iter_mut().filter(|p| p.name == old) {
                p.name = new.clone();
                if let Some(t) = &ty {
                    p.port_type = t.clone();
                }
            }
            for w in gd.wires.iter_mut() {
                if Some(w.to_node) == go && w.to_port == old {
                    w.to_port = new.clone();
                }
            }
            for w in parent_wires.iter_mut() {
                if w.from_node == gid && w.from_port == old {
                    w.from_port = new.clone();
                }
            }
        }
    }
}
