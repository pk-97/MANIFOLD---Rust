//! v1.11.0 → v1.12.0: `node.render_scene` per-object transform params
//! synthesize into `node.transform_3d` atoms wired through the new
//! `transform_n: Transform` port. See
//! `docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` §2 D3/D4 for the design;
//! this doc comment covers mechanics only.
//!
//! Pure `Value → Value`, pre-typed-deserialize (the quarantine rule — this
//! module never consults the live primitive registry, only the JSON tree
//! and the fixed base-name table below). Walks every graph-shaped JSON
//! object in the project: each preset instance's `graph`
//! (effect)/`genParams.graph` (generator) via the shared
//! `for_each_preset_instance` walk, AND every `embeddedPresets[].def` (D4:
//! "every instance graph... + embeddedPresets defs").
//!
//! For each `node.render_scene` node found — recursing through nested node
//! groups, since an imported model's per-object producers live inside one
//! (`docs/GROUPING_GRAPHS.md`) — scans its `params` for the nine legacy
//! per-object TRS keys (`pos_x_{k}`, …, `scale_z_{k}`) for every object
//! index `k` present. For each such object:
//!
//!   1. synthesizes a `node.transform_3d` node carrying the found values
//!      (a missing family falls through to the atom's own default — same
//!      semantics the old scattered params had);
//!   2. wires its `transform` output into the render node's `transform_{k}`
//!      port — INSIDE the group that produces `mesh_{k}` when a same-level
//!      wire traces `mesh_{k}` to a group's `vertices` output (the importer
//!      shape: the transform node joins the object's own box, and the
//!      group's interface gains a `transform` output feeding a top-level
//!      `group.transform → render.transform_{k}` wire), or at the render
//!      node's own level otherwise;
//!   3. deletes the nine legacy keys — and any matching `exposedParams`
//!      entry, migrated to the new node under its un-suffixed name — from
//!      the render node;
//!   4. re-points any `BindingDef` targeting the render node's
//!      `pos_x_{k}`-family params (by `nodeId`, or by the legacy `handle`
//!      form for a pre-node-id document) to the synthesized node's
//!      un-suffixed params, so a card slider keeps driving the same value.
//!
//! Failure story: a malformed per-object param value is dropped with a
//! warning; the rest of that object's transform (and every other object)
//! still migrates — the migration never aborts a load (D4).
//!
//! Placement is decided per graph LEVEL (top-level `nodes`/`wires`, or one
//! nested group's body `nodes`/`wires`) — the render node and the group
//! producing its `mesh_{k}` are always siblings at the same level in every
//! shape this codebase has ever emitted (see `gltf_import.rs`'s
//! `build_import_graph`, whose top-level `nodes` holds the object groups
//! AND the shared `render_scene` node side by side). Node `id` (the u32
//! wire-endpoint key) and `nodeId` (the stable string binding target) are
//! both a GLOBAL namespace across the whole document, including every
//! nested group body — `gltf_import.rs`'s single `fresh_id` counter spans
//! both levels the same way — so a fresh id/nodeId is minted once up front
//! from a whole-document scan, not per-level.

use std::collections::{BTreeSet, HashSet};

use serde_json::{Map, Value};

/// The nine legacy per-object TRS param base names. Order is cosmetic
/// (matches the retired `render_scene::rebuild`'s emission order).
const TRS_BASE_NAMES: &[&str] = &[
    "pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "scale_x", "scale_y", "scale_z",
];

const RENDER_SCENE_TYPE_ID: &str = "node.render_scene";
const TRANSFORM_3D_TYPE_ID: &str = "node.transform_3d";
const GROUP_TYPE_ID: &str = "group";
const GROUP_OUTPUT_TYPE_ID: &str = "system.group_output";

/// Entry point wired into `crate::migrate::migrate_if_needed`'s 1.12.0 rung.
pub(crate) fn migrate(root: &mut Value) {
    crate::migrate::for_each_preset_instance(root, |fx| {
        let Value::Object(map) = fx else { return };
        if let Some(graph) = map.get_mut("graph") {
            migrate_graph_value(graph);
        }
    });
    migrate_embedded_presets(root);
}

/// `embeddedPresets[].def` carries the same `nodes`/`wires`/`presetMetadata`
/// shape as an instance's `graph` — walk it identically (D4).
fn migrate_embedded_presets(root: &mut Value) {
    let Some(presets) = root.get_mut("embeddedPresets").and_then(|v| v.as_array_mut()) else {
        return;
    };
    for preset in presets.iter_mut() {
        if let Some(def) = preset.get_mut("def") {
            migrate_graph_value(def);
        }
    }
}

/// One render_scene object migrated: enough identity to re-point a binding
/// that used to target `{old_node_id}.{base}_{k}`.
struct RenameRecord {
    old_node_id: String,
    old_handle: Option<String>,
    k: u32,
    new_node_id: String,
}

/// Monotonic id/nodeId minter, seeded from a whole-document scan so a
/// synthesized node never collides with anything already in the document —
/// including inside nested group bodies, which share the same namespace.
struct IdGen {
    next_id: u32,
    used_node_ids: HashSet<String>,
}

impl IdGen {
    fn next_id(&mut self) -> u32 {
        let v = self.next_id;
        self.next_id += 1;
        v
    }

    /// Mint a fresh, reserved node id string, preferring `base` verbatim and
    /// falling back to `{base}_2`, `{base}_3`, … on collision.
    fn fresh_node_id(&mut self, base: &str) -> String {
        if self.used_node_ids.insert(base.to_string()) {
            return base.to_string();
        }
        let mut n = 2u32;
        loop {
            let candidate = format!("{base}_{n}");
            if self.used_node_ids.insert(candidate.clone()) {
                return candidate;
            }
            n += 1;
        }
    }
}

/// Migrate one graph-shaped JSON value (an instance's `graph` or an
/// embedded preset's `def`): recurse the node tree looking for
/// `node.render_scene`, then re-point this graph's own
/// `presetMetadata.bindings` for every object migrated.
fn migrate_graph_value(graph: &mut Value) {
    let Value::Object(graph_map) = graph else { return };

    let mut next_id: u32 = 0;
    let mut used_node_ids: HashSet<String> = HashSet::new();
    if let Some(Value::Array(nodes)) = graph_map.get("nodes") {
        scan_ids(nodes, &mut next_id, &mut used_node_ids);
    }

    let Some(Value::Array(mut nodes)) = graph_map.remove("nodes") else {
        return;
    };
    let mut wires = match graph_map.remove("wires") {
        Some(Value::Array(w)) => w,
        _ => Vec::new(),
    };

    let mut id_gen = IdGen { next_id, used_node_ids };
    let renames = migrate_level(&mut nodes, &mut wires, &mut id_gen);

    graph_map.insert("nodes".to_string(), Value::Array(nodes));
    graph_map.insert("wires".to_string(), Value::Array(wires));

    if !renames.is_empty() {
        repoint_bindings(graph_map, &renames);
    }
}

/// Recursively collect the max existing `id` (+1) and every `nodeId` /
/// `handle` string anywhere in the node tree (top level and every nested
/// group body) — the whole-document namespace a fresh id/nodeId must avoid.
/// `handle` is reserved too, defensively: a pre-node-id document's
/// effective addressing falls back to `handle` (see `EffectGraphNode::node_id`'s
/// doc comment), so treating it as taken avoids a theoretical collision.
fn scan_ids(nodes: &[Value], next_id: &mut u32, used_node_ids: &mut HashSet<String>) {
    for node in nodes {
        if let Some(id) = node.get("id").and_then(|v| v.as_u64()) {
            *next_id = (*next_id).max(id as u32 + 1);
        }
        if let Some(nid) = node.get("nodeId").and_then(|v| v.as_str()) {
            used_node_ids.insert(nid.to_string());
        }
        if let Some(handle) = node.get("handle").and_then(|v| v.as_str()) {
            used_node_ids.insert(handle.to_string());
        }
        if let Some(inner) = node
            .get("group")
            .and_then(|g| g.get("nodes"))
            .and_then(|v| v.as_array())
        {
            scan_ids(inner, next_id, used_node_ids);
        }
    }
}

/// Migrate one level (top-level graph, or one group's body): recurse into
/// every child group FIRST (so a render_scene nested inside a group is
/// covered too), then migrate every `node.render_scene` node found AT this
/// level. Returns every object migrated at or below this level, so the one
/// caller-level `presetMetadata.bindings` re-point sees all of them.
fn migrate_level(nodes: &mut Vec<Value>, wires: &mut Vec<Value>, id_gen: &mut IdGen) -> Vec<RenameRecord> {
    let mut renames = Vec::new();

    for node in nodes.iter_mut() {
        let is_group = node.get("typeId").and_then(|v| v.as_str()) == Some(GROUP_TYPE_ID);
        if !is_group {
            continue;
        }
        let Some(Value::Object(group_map)) = node.get_mut("group") else {
            continue;
        };
        let mut inner_nodes = match group_map.remove("nodes") {
            Some(Value::Array(n)) => n,
            _ => Vec::new(),
        };
        let mut inner_wires = match group_map.remove("wires") {
            Some(Value::Array(w)) => w,
            _ => Vec::new(),
        };
        let mut inner_renames = migrate_level(&mut inner_nodes, &mut inner_wires, id_gen);
        group_map.insert("nodes".to_string(), Value::Array(inner_nodes));
        group_map.insert("wires".to_string(), Value::Array(inner_wires));
        renames.append(&mut inner_renames);
    }

    let render_scene_indices: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.get("typeId").and_then(|v| v.as_str()) == Some(RENDER_SCENE_TYPE_ID))
        .map(|(i, _)| i)
        .collect();

    for idx in render_scene_indices {
        migrate_render_scene_node(nodes, wires, idx, id_gen, &mut renames);
    }

    renames
}

/// Migrate every legacy-transform object on ONE `node.render_scene` node at
/// index `render_idx` of `nodes` (sibling to `wires` at this same level).
fn migrate_render_scene_node(
    nodes: &mut Vec<Value>,
    wires: &mut Vec<Value>,
    render_idx: usize,
    id_gen: &mut IdGen,
    renames: &mut Vec<RenameRecord>,
) {
    let Some(render_id_u32) = nodes[render_idx].get("id").and_then(|v| v.as_u64()).map(|v| v as u32)
    else {
        return;
    };
    let render_node_id = nodes[render_idx]
        .get("nodeId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let render_handle = nodes[render_idx]
        .get("handle")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let object_indices: Vec<u32> = {
        let Some(params) = nodes[render_idx].get("params").and_then(|v| v.as_object()) else {
            return;
        };
        let mut ks: BTreeSet<u32> = BTreeSet::new();
        for key in params.keys() {
            for base in TRS_BASE_NAMES {
                if let Some(rest) = key.strip_prefix(&format!("{base}_"))
                    && let Ok(k) = rest.parse::<u32>()
                {
                    ks.insert(k);
                }
            }
        }
        ks.into_iter().collect()
    };
    if object_indices.is_empty() {
        return;
    }

    for k in object_indices {
        migrate_one_object(
            nodes,
            wires,
            render_idx,
            render_id_u32,
            &render_node_id,
            render_handle.as_deref(),
            k,
            id_gen,
            renames,
        );
    }
}

/// Migrate object `k` of one render_scene node: pull its (up to) nine
/// legacy params off the render node, synthesize a `node.transform_3d`
/// carrying them, wire it in (inside the producing group when found, else
/// at this level), and record the rename for the binding pass.
#[allow(clippy::too_many_arguments)]
fn migrate_one_object(
    nodes: &mut Vec<Value>,
    wires: &mut Vec<Value>,
    render_idx: usize,
    render_id_u32: u32,
    render_node_id: &str,
    render_handle: Option<&str>,
    k: u32,
    id_gen: &mut IdGen,
    renames: &mut Vec<RenameRecord>,
) {
    let mut new_params: Map<String, Value> = Map::new();
    let mut new_exposed: Vec<String> = Vec::new();
    {
        let Value::Object(render_map) = &mut nodes[render_idx] else {
            return;
        };
        for base in TRS_BASE_NAMES {
            let key = format!("{base}_{k}");
            let removed = render_map
                .get_mut("params")
                .and_then(|v| v.as_object_mut())
                .and_then(|m| m.remove(&key));
            let Some(v) = removed else { continue };

            let is_valid_float = matches!(&v, Value::Object(m)
                if m.get("type").and_then(|t| t.as_str()) == Some("Float")
                    && m.get("value").and_then(|x| x.as_f64()).is_some());
            if is_valid_float {
                new_params.insert((*base).to_string(), v);
            } else {
                eprintln!(
                    "[scene_transform_v1120] WARNING: render_scene node '{render_node_id}' \
                     object {k} param '{key}' has an unrecognized shape ({v:?}) — dropped; \
                     the rest of this object's transform still migrates."
                );
            }

            // exposedParams carries the SAME key string. Drop it here
            // regardless (an exposed slot for a dropped param would
            // dangle), and carry it to the new node only when the value
            // itself migrated cleanly.
            if let Some(exposed) = render_map.get_mut("exposedParams").and_then(|v| v.as_array_mut())
                && let Some(pos) = exposed.iter().position(|e| e.as_str() == Some(key.as_str()))
            {
                exposed.remove(pos);
                if is_valid_float {
                    new_exposed.push((*base).to_string());
                }
            }
        }
    }

    if new_params.is_empty() {
        // Every one of this object's legacy params was malformed — nothing
        // transferable; the render node is already clean (each key was
        // still removed above), so there's nothing left to wire.
        return;
    }

    let new_id = id_gen.next_id();
    let handle = format!("transform_{k}");
    let new_node_id = id_gen.fresh_node_id(&handle);

    let mut node_obj = Map::new();
    node_obj.insert("id".to_string(), Value::from(new_id));
    node_obj.insert("nodeId".to_string(), Value::from(new_node_id.clone()));
    node_obj.insert("typeId".to_string(), Value::from(TRANSFORM_3D_TYPE_ID));
    node_obj.insert("handle".to_string(), Value::from(handle));
    node_obj.insert("params".to_string(), Value::Object(new_params));
    if !new_exposed.is_empty() {
        node_obj.insert(
            "exposedParams".to_string(),
            Value::Array(new_exposed.into_iter().map(Value::String).collect()),
        );
    }
    let new_node = Value::Object(node_obj);

    let mesh_port = format!("mesh_{k}");
    let producer_group_idx: Option<usize> = wires.iter().find_map(|w| {
        let wm = w.as_object()?;
        let to_node = wm.get("toNode").and_then(|v| v.as_u64())? as u32;
        let to_port = wm.get("toPort").and_then(|v| v.as_str())?;
        if to_node != render_id_u32 || to_port != mesh_port {
            return None;
        }
        let from_node = wm.get("fromNode").and_then(|v| v.as_u64())? as u32;
        nodes.iter().position(|n| {
            n.get("id").and_then(|v| v.as_u64()) == Some(u64::from(from_node))
                && n.get("typeId").and_then(|v| v.as_str()) == Some(GROUP_TYPE_ID)
        })
    });

    let mut placed_in_group = false;
    if let Some(group_idx) = producer_group_idx {
        placed_in_group = place_inside_group(nodes, wires, group_idx, new_node.clone(), new_id, render_id_u32, k);
    }
    if !placed_in_group {
        nodes.push(new_node);
        wires.push(wire_json(new_id, "transform", render_id_u32, &format!("transform_{k}")));
    }

    renames.push(RenameRecord {
        old_node_id: render_node_id.to_string(),
        old_handle: render_handle.map(|s| s.to_string()),
        k,
        new_node_id,
    });
}

/// Inject `new_node` inside `nodes[group_idx]`'s body (wired to its
/// `system.group_output` boundary node, extending the group's outward
/// `interface.outputs` with a `transform` port), and add the OUTER wire
/// from the group's own (now-extended) output to the render node's
/// `transform_{k}` port at THIS level. Returns `false` (declining
/// group placement, so the caller falls back to top-level) when the
/// group's body is malformed (no `group` object, or no boundary node) —
/// never silently drops the transform.
fn place_inside_group(
    nodes: &mut [Value],
    wires: &mut Vec<Value>,
    group_idx: usize,
    new_node: Value,
    new_id: u32,
    render_id_u32: u32,
    k: u32,
) -> bool {
    let Some(group_outer_id) = nodes[group_idx].get("id").and_then(|v| v.as_u64()).map(|v| v as u32)
    else {
        return false;
    };
    let Value::Object(group_node_map) = &mut nodes[group_idx] else {
        return false;
    };
    let Some(Value::Object(group_def)) = group_node_map.get_mut("group") else {
        return false;
    };

    let out_node_local_id = group_def
        .get("nodes")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|n| n.get("typeId").and_then(|v| v.as_str()) == Some(GROUP_OUTPUT_TYPE_ID))
        })
        .and_then(|n| n.get("id"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
    let Some(out_node_local_id) = out_node_local_id else {
        return false;
    };

    if let Some(Value::Array(inner_nodes)) = group_def.get_mut("nodes") {
        inner_nodes.push(new_node);
    } else {
        return false;
    }
    let inner_wire = wire_json(new_id, "transform", out_node_local_id, "transform");
    match group_def.get_mut("wires") {
        Some(Value::Array(inner_wires)) => inner_wires.push(inner_wire),
        _ => return false,
    }

    if let Some(interface) = group_def.get_mut("interface").and_then(|v| v.as_object_mut()) {
        let outputs = interface
            .entry("outputs")
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(arr) = outputs.as_array_mut() {
            let already = arr
                .iter()
                .any(|o| o.get("name").and_then(|v| v.as_str()) == Some("transform"));
            if !already {
                arr.push(serde_json::json!({ "name": "transform", "portType": "Transform" }));
            }
        }
    }

    wires.push(wire_json(group_outer_id, "transform", render_id_u32, &format!("transform_{k}")));
    true
}

fn wire_json(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> Value {
    serde_json::json!({
        "fromNode": from_node,
        "fromPort": from_port,
        "toNode": to_node,
        "toPort": to_port,
    })
}

/// Re-point `graph_map["presetMetadata"]["bindings"]` entries that targeted
/// a migrated render_scene param to the synthesized node's un-suffixed
/// param. Matches by `nodeId` (the primary, stable path) or by the legacy
/// `handleNode` form's `handle` (pre-node-id documents) — the latter is a
/// defensive fallback: `handle` isn't guaranteed globally unique the way
/// `nodeId` is, but a genuine collision between two DIFFERENT render_scene
/// nodes sharing a handle string is exactly the kind of pathological legacy
/// case this fallback exists for in the first place, not a regression this
/// migration introduces.
fn repoint_bindings(graph_map: &mut Map<String, Value>, renames: &[RenameRecord]) {
    let Some(meta) = graph_map.get_mut("presetMetadata").and_then(|v| v.as_object_mut()) else {
        return;
    };
    let Some(Value::Array(bindings)) = meta.get_mut("bindings") else {
        return;
    };

    // (target key, old param) -> (new nodeId, new param). Built once so a
    // binding's lookup is a single hash query regardless of renames.len().
    let mut lookup: std::collections::HashMap<(String, String), (String, String)> =
        std::collections::HashMap::new();
    for r in renames {
        for base in TRS_BASE_NAMES {
            let old_param = format!("{base}_{}", r.k);
            lookup.insert(
                (r.old_node_id.clone(), old_param.clone()),
                (r.new_node_id.clone(), (*base).to_string()),
            );
            if let Some(h) = &r.old_handle {
                lookup.insert(
                    (h.clone(), old_param),
                    (r.new_node_id.clone(), (*base).to_string()),
                );
            }
        }
    }

    for binding in bindings.iter_mut() {
        let Some(target) = binding.get_mut("target").and_then(|v| v.as_object_mut()) else {
            continue;
        };
        let Some(kind) = target.get("kind").and_then(|v| v.as_str()).map(str::to_string) else {
            continue;
        };
        let target_key = match kind.as_str() {
            "node" => target.get("nodeId").and_then(|v| v.as_str()).map(str::to_string),
            "handleNode" => target.get("handle").and_then(|v| v.as_str()).map(str::to_string),
            _ => None,
        };
        let Some(target_key) = target_key else { continue };
        let Some(param) = target.get("param").and_then(|v| v.as_str()).map(str::to_string) else {
            continue;
        };

        if let Some((new_node_id, new_param)) = lookup.get(&(target_key, param)) {
            target.insert("kind".to_string(), Value::from("node"));
            target.insert("nodeId".to_string(), Value::from(new_node_id.clone()));
            target.remove("handle");
            target.insert("param".to_string(), Value::from(new_param.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal fixture: ONE render_scene node at the top level, no producer
    /// groups at all — object 0's transform migrates to a top-level
    /// `node.transform_3d` (placement falls back correctly when there's no
    /// group to place inside).
    fn top_level_fixture() -> Value {
        serde_json::json!({
            "version": 1,
            "nodes": [
                {
                    "id": 0, "nodeId": "render", "typeId": "node.render_scene", "handle": "render",
                    "params": {
                        "objects": {"type":"Int","value":1},
                        "lights": {"type":"Int","value":1},
                        "pos_x_0": {"type":"Float","value": 2.5},
                        "rot_y_0": {"type":"Float","value": 0.75},
                        "scale_z_0": {"type":"Float","value": 1.5}
                    },
                    "exposedParams": ["pos_x_0"]
                }
            ],
            "wires": []
        })
    }

    #[test]
    fn top_level_object_with_no_producer_group_migrates_to_top_level_transform_node() {
        let mut graph = top_level_fixture();
        migrate_graph_value(&mut graph);

        let nodes = graph["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2, "render_scene + one synthesized transform node");
        let render = nodes.iter().find(|n| n["nodeId"] == "render").unwrap();
        assert!(render["params"].get("pos_x_0").is_none(), "legacy param removed");
        assert!(render["params"].get("rot_y_0").is_none());
        assert!(render["params"].get("scale_z_0").is_none());
        assert_eq!(render["params"]["objects"]["value"].as_f64(), Some(1.0), "unrelated params untouched");
        assert!(
            render.get("exposedParams").and_then(|v| v.as_array()).map(|a| a.is_empty()).unwrap_or(true),
            "exposed pos_x_0 entry removed from render node"
        );

        let t = nodes.iter().find(|n| n["typeId"] == "node.transform_3d").unwrap();
        assert_eq!(t["handle"], "transform_0");
        assert_eq!(t["params"]["pos_x"]["value"].as_f64(), Some(2.5));
        assert_eq!(t["params"]["rot_y"]["value"].as_f64(), Some(0.75));
        assert_eq!(t["params"]["scale_z"]["value"].as_f64(), Some(1.5));
        assert_eq!(t["exposedParams"][0], "pos_x", "exposed slot carried over, un-suffixed");

        let wires = graph["wires"].as_array().unwrap();
        let t_id = t["id"].as_u64().unwrap();
        assert!(
            wires.iter().any(|w| w["fromNode"].as_u64() == Some(t_id)
                && w["fromPort"] == "transform"
                && w["toNode"].as_u64() == Some(0)
                && w["toPort"] == "transform_0"),
            "new node's transform output wires into render's transform_0 port"
        );
    }

    /// Malformed-value case: object 2 carries one valid param and one
    /// garbage-shaped param. The valid one migrates; the malformed one is
    /// dropped with a warning; nothing panics; the render node ends up
    /// clean either way (both keys removed).
    #[test]
    fn malformed_transform_value_is_dropped_the_rest_still_migrates() {
        let mut graph = serde_json::json!({
            "version": 1,
            "nodes": [
                {
                    "id": 0, "nodeId": "render", "typeId": "node.render_scene", "handle": "render",
                    "params": {
                        "objects": {"type":"Int","value":3},
                        "pos_x_2": {"type":"Float","value": 5.0},
                        "rot_y_2": {"type":"String","value": "not-a-number"}
                    }
                }
            ],
            "wires": []
        });

        migrate_graph_value(&mut graph);

        let nodes = graph["nodes"].as_array().unwrap();
        let render = nodes.iter().find(|n| n["nodeId"] == "render").unwrap();
        assert!(render["params"].get("pos_x_2").is_none());
        assert!(render["params"].get("rot_y_2").is_none(), "malformed key still removed, not left dangling");

        let t = nodes.iter().find(|n| n["typeId"] == "node.transform_3d").unwrap();
        assert_eq!(t["params"]["pos_x"]["value"].as_f64(), Some(5.0));
        assert!(t["params"].get("rot_y").is_none(), "malformed value never transferred");
        assert_eq!(t["handle"], "transform_2");
    }

    /// The load-bearing shape: an object's producers live inside a named
    /// group (the importer's own layout). The synthesized transform node
    /// must join that SAME group (not sit top-level beside render_scene),
    /// the group's interface gains a `transform` output wired from the new
    /// node to the group's `system.group_output` boundary, and the OUTER
    /// wire connects the group's new output straight to render's
    /// `transform_{k}` port.
    fn grouped_fixture() -> Value {
        serde_json::json!({
            "version": 1,
            "nodes": [
                {
                    "id": 0, "nodeId": "leaf_group", "typeId": "group", "handle": "Leaf",
                    "group": {
                        "interface": {
                            "inputs": [],
                            "outputs": [
                                {"name": "vertices", "portType": "Array(Vertex)"},
                                {"name": "material", "portType": "Material"}
                            ],
                            "params": []
                        },
                        "nodes": [
                            {"id": 3, "nodeId": "mesh_0", "typeId": "node.gltf_mesh_source", "handle": "mesh_0"},
                            {"id": 4, "nodeId": "mat_0", "typeId": "node.pbr_material", "handle": "mat_0"},
                            {"id": 5, "nodeId": "leaf_out", "typeId": "system.group_output", "handle": "output"}
                        ],
                        "wires": [
                            {"fromNode": 3, "fromPort": "vertices", "toNode": 5, "toPort": "vertices"},
                            {"fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "material"}
                        ]
                    }
                },
                {
                    "id": 1, "nodeId": "bark_group", "typeId": "group", "handle": "Bark",
                    "group": {
                        "interface": {
                            "inputs": [],
                            "outputs": [
                                {"name": "vertices", "portType": "Array(Vertex)"},
                                {"name": "material", "portType": "Material"}
                            ],
                            "params": []
                        },
                        "nodes": [
                            {"id": 6, "nodeId": "mesh_1", "typeId": "node.gltf_mesh_source", "handle": "mesh_1"},
                            {"id": 7, "nodeId": "mat_1", "typeId": "node.pbr_material", "handle": "mat_1"},
                            {"id": 8, "nodeId": "bark_out", "typeId": "system.group_output", "handle": "output"}
                        ],
                        "wires": [
                            {"fromNode": 6, "fromPort": "vertices", "toNode": 8, "toPort": "vertices"},
                            {"fromNode": 7, "fromPort": "out", "toNode": 8, "toPort": "material"}
                        ]
                    }
                },
                {
                    "id": 2, "nodeId": "render", "typeId": "node.render_scene", "handle": "render",
                    "params": {
                        "objects": {"type":"Int","value":2},
                        "lights": {"type":"Int","value":1},
                        "pos_x_0": {"type":"Float","value": -1.0},
                        "pos_x_1": {"type":"Float","value": 2.5},
                        "rot_y_1": {"type":"Float","value": 0.5}
                    },
                    "exposedParams": ["pos_x_1"]
                }
            ],
            "wires": [
                {"fromNode": 0, "fromPort": "vertices", "toNode": 2, "toPort": "mesh_0"},
                {"fromNode": 0, "fromPort": "material", "toNode": 2, "toPort": "material_0"},
                {"fromNode": 1, "fromPort": "vertices", "toNode": 2, "toPort": "mesh_1"},
                {"fromNode": 1, "fromPort": "material", "toNode": 2, "toPort": "material_1"}
            ],
            "presetMetadata": {
                "id": "Test",
                "displayName": "Test",
                "category": "Geometry",
                "oscPrefix": "test",
                "params": [
                    {"id": "obj2_x", "name": "Object 2 X", "min": -10.0, "max": 10.0, "defaultValue": 2.5}
                ],
                "bindings": [
                    {
                        "id": "obj2_x", "label": "Object 2 X", "defaultValue": 2.5,
                        "target": {"kind": "node", "nodeId": "render", "param": "pos_x_1"}
                    }
                ]
            }
        })
    }

    #[test]
    fn object_traced_to_its_group_places_transform_inside_it_and_extends_interface() {
        let mut graph = grouped_fixture();
        migrate_graph_value(&mut graph);

        let nodes = graph["nodes"].as_array().unwrap();
        // Still exactly 3 top-level nodes — nothing landed top-level.
        assert_eq!(nodes.len(), 3, "no top-level transform node — both objects placed inside their groups");

        let leaf = nodes.iter().find(|n| n["nodeId"] == "leaf_group").unwrap();
        let leaf_inner = leaf["group"]["nodes"].as_array().unwrap();
        assert!(
            leaf_inner.iter().any(|n| n["typeId"] == "node.transform_3d" && n["handle"] == "transform_0"),
            "Leaf's own transform node lives inside its group"
        );
        let leaf_t = leaf_inner.iter().find(|n| n["typeId"] == "node.transform_3d").unwrap();
        assert_eq!(leaf_t["params"]["pos_x"]["value"].as_f64(), Some(-1.0));

        let leaf_outputs = leaf["group"]["interface"]["outputs"].as_array().unwrap();
        assert!(
            leaf_outputs.iter().any(|o| o["name"] == "transform" && o["portType"] == "Transform"),
            "Leaf's interface gains a transform output"
        );
        let leaf_out_id = leaf["group"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["typeId"] == "system.group_output")
            .unwrap()["id"]
            .as_u64()
            .unwrap();
        let leaf_inner_wires = leaf["group"]["wires"].as_array().unwrap();
        assert!(
            leaf_inner_wires.iter().any(|w| w["fromPort"] == "transform"
                && w["toNode"].as_u64() == Some(leaf_out_id)
                && w["toPort"] == "transform"),
            "the new node wires into Leaf's boundary output"
        );

        let bark = nodes.iter().find(|n| n["nodeId"] == "bark_group").unwrap();
        let bark_t = bark["group"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["typeId"] == "node.transform_3d")
            .unwrap();
        assert_eq!(bark_t["handle"], "transform_1");
        assert_eq!(bark_t["params"]["pos_x"]["value"].as_f64(), Some(2.5));
        assert_eq!(bark_t["params"]["rot_y"]["value"].as_f64(), Some(0.5));
        assert_eq!(bark_t["exposedParams"][0], "pos_x", "exposed slot carried into Bark's inner node");

        // Outer wires: each group's OWN "transform" output feeds render's
        // transform_k port directly (crossing the group boundary at the top
        // level, same as the mesh_k/material_k wires already do).
        let outer_wires = graph["wires"].as_array().unwrap();
        let render_id = 2u64;
        let leaf_group_id = leaf["id"].as_u64().unwrap();
        let bark_group_id = bark["id"].as_u64().unwrap();
        assert!(outer_wires.iter().any(|w| w["fromNode"].as_u64() == Some(leaf_group_id)
            && w["fromPort"] == "transform" && w["toNode"].as_u64() == Some(render_id) && w["toPort"] == "transform_0"));
        assert!(outer_wires.iter().any(|w| w["fromNode"].as_u64() == Some(bark_group_id)
            && w["fromPort"] == "transform" && w["toNode"].as_u64() == Some(render_id) && w["toPort"] == "transform_1"));

        // Render node itself is clean.
        let render = nodes.iter().find(|n| n["nodeId"] == "render").unwrap();
        assert!(render["params"].get("pos_x_0").is_none());
        assert!(render["params"].get("pos_x_1").is_none());
        assert!(render["params"].get("rot_y_1").is_none());

        // The card binding that used to target render.pos_x_1 now targets
        // Bark's synthesized node's un-suffixed pos_x — the slider keeps
        // driving the same value with the same public binding id.
        let binding = &graph["presetMetadata"]["bindings"][0];
        assert_eq!(binding["id"], "obj2_x", "public binding id (what drivers/OSC address) unchanged");
        assert_eq!(binding["target"]["kind"], "node");
        assert_eq!(binding["target"]["nodeId"], bark_t["nodeId"]);
        assert_eq!(binding["target"]["param"], "pos_x");
    }

    /// Whole-chain idempotency: running the migration twice on the same
    /// document is a no-op the second time (no legacy keys remain to scan).
    #[test]
    fn second_run_is_a_no_op() {
        let mut graph = grouped_fixture();
        migrate_graph_value(&mut graph);
        let once = graph.clone();
        migrate_graph_value(&mut graph);
        assert_eq!(graph, once, "idempotent: nothing left to migrate on a second pass");
    }

    /// `embeddedPresets[].def` is walked with the same migration (D4).
    #[test]
    fn embedded_preset_defs_are_migrated_too() {
        let mut root = serde_json::json!({
            "projectVersion": "1.11.0",
            "embeddedPresets": [
                { "kind": "generator", "origin": "saved", "def": top_level_fixture() }
            ]
        });
        migrate(&mut root);
        let def = &root["embeddedPresets"][0]["def"];
        let nodes = def["nodes"].as_array().unwrap();
        assert!(
            nodes.iter().any(|n| n["typeId"] == "node.transform_3d"),
            "embedded preset def's render_scene node was migrated"
        );
    }

    /// Legacy `handleNode` binding form (pre-node-id document): the target
    /// carries `handle` instead of `nodeId`. Re-pointing must still find it.
    #[test]
    fn legacy_handle_node_binding_repoints_by_handle() {
        let mut graph = serde_json::json!({
            "version": 1,
            "nodes": [
                {
                    "id": 0, "nodeId": "", "typeId": "node.render_scene", "handle": "render",
                    "params": {
                        "pos_x_0": {"type":"Float","value": 9.0}
                    }
                }
            ],
            "wires": [],
            "presetMetadata": {
                "id": "Test", "displayName": "Test", "category": "Geometry", "oscPrefix": "test",
                "params": [],
                "bindings": [
                    {
                        "id": "user.slider.1", "label": "Slider", "defaultValue": 9.0,
                        "target": {"kind": "handleNode", "handle": "render", "param": "pos_x_0"}
                    }
                ]
            }
        });
        migrate_graph_value(&mut graph);
        let binding = &graph["presetMetadata"]["bindings"][0];
        assert_eq!(binding["target"]["kind"], "node", "upgraded to the node form");
        assert_eq!(binding["target"]["param"], "pos_x");
        let t = graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["typeId"] == "node.transform_3d")
            .unwrap();
        assert_eq!(binding["target"]["nodeId"], t["nodeId"]);
    }

    /// No render_scene node at all, or a render_scene with no legacy
    /// params: both are clean no-ops (nothing added, nothing removed).
    #[test]
    fn no_render_scene_or_no_legacy_params_is_a_no_op() {
        let mut graph = serde_json::json!({
            "version": 1,
            "nodes": [
                {"id": 0, "nodeId": "render", "typeId": "node.render_scene", "handle": "render",
                 "params": {"objects": {"type":"Int","value":1}}}
            ],
            "wires": []
        });
        let before = graph.clone();
        migrate_graph_value(&mut graph);
        assert_eq!(graph, before);
    }
}
