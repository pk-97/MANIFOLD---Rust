//! v1.13.0 → v1.14.0: calibrate every stored `coc_from_depth` graph to its
//! scene scale (BUG-bdwd — DoF blur overpowering below f_stop 64).
//!
//! The CoC shader used to hardcode `WORLD_TO_MM = 1000.0` (1 world unit = 1
//! meter). Imported models come in any scale, so their stored
//! `focus_distance` / `depth` values read through the lens physics as a
//! physically-mismatched camera: a sub-meter-scale scene (Peter's live
//! scenes sit at camera distance 0–1) got D_mm/S_mm tiny and the formula
//! exploded CoC past max_radius at EVERY aperture — hence f_stop had to be
//! cranked to 64+ to see anything. The fix (this rung + the import stamp)
//! adds a `world_to_mm` param to `node.coc_from_depth` calibrated per scene:
//! `world_to_mm = 1000 / scene_radius`, so the scene's model units read as
//! real meter-scale distances.
//!
//! Look-preservation: the CoC formula is homogeneously degree-1 in
//! `world_to_mm / f_stop`. So multiplying a stored `f_stop` by the scene
//! radius R and calibrating `world_to_mm = 1000/R` keeps the SAME physical
//! CoC (the `max(s_mm − f_mm, 1.0)` denominator floor makes it approximate
//! across depth regimes, within a few percent — see the migration test's
//! tolerance). That is exactly what this rung does, per stored graph:
//!
//!   - every graph-shaped value (layer genParams.graph, effect clips,
//!     embeddedPresets[].def, … via `for_each_preset_instance`) that
//!     CONTAINS a `node.coc_from_depth` node AND carries a present
//!     `presetMetadata.sceneBounds` (the import's (min, max) bbox, radius R
//!     = half the diagonal) gets `world_to_mm = 1000 / R` stamped onto its
//!     coc node's params AND every stored `f_stop` param multiplied by R
//!     (the lens's tuned value, so the look survives);
//!   - a graph with NO `sceneBounds`, or no coc_from_depth node, is a
//!     byte-identical passthrough (default world_to_mm = 1000.0 = old
//!     behavior, so nothing to migrate — CinematicScene and hand-built
//!     presets fall here and keep their exact tuned values).
//!
//! The rung is idempotent by construction: a migrated graph carries
//! `world_to_mm` on its coc node, and the remigration guard skips graphs
//! that already have the param (a second load is a no-op, same as the tail
//! migration's marker check).

use serde_json::Value;

const COC: &str = "node.coc_from_depth";
const SCENE_BOUNDS: &str = "sceneBounds";

/// One stored graph's scene radius, from its `presetMetadata.sceneBounds`
/// `(min, max)` bbox — the same half-diagonal the import derives. `None`
/// when the graph carries no sceneBounds (presets that aren't scene imports
/// fall back to world_to_mm=1000, the old constant, and byte-identical
/// behavior).
fn scene_radius_from_meta(meta: &Value) -> Option<f32> {
    let bounds = meta.get(SCENE_BOUNDS)?;
    let min = bounds.get(0)?.as_array()?;
    let max = bounds.get(1)?.as_array()?;
    let min = [min.first()?.as_f64()? as f32, min.get(1)?.as_f64()? as f32, min.get(2)?.as_f64()? as f32];
    let max = [max.first()?.as_f64()? as f32, max.get(1)?.as_f64()? as f32, max.get(2)?.as_f64()? as f32];
    let dims = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    let radius =
        ((dims[0] * dims[0] + dims[1] * dims[1] + dims[2] * dims[2]).sqrt() * 0.5).max(0.01);
    Some(radius)
}

/// Recursively walk a node array (including group interiors) and return the
/// flattened list of nodes. The coc node lives inside the `dof` group on
/// import-assembled graphs (see `gltf_import/cinematic_tail.rs`).
fn flatten_nodes(nodes: &[Value]) -> Vec<&Value> {
    let mut out = Vec::new();
    fn walk<'a>(nodes: &'a [Value], out: &mut Vec<&'a Value>) {
        for n in nodes {
            out.push(n);
            if let Some(group) = n.get("group").and_then(|g| g.get("nodes")).and_then(|ns| ns.as_array())
            {
                walk(group, out);
            }
        }
    }
    walk(nodes, &mut out);
    out
}

/// Recursively visit every node (including group interiors) with `&mut`.
/// Caller's closure runs on the node, then the walk descends into the
/// node's group body — node-by-node, so no two mutable borrows coexist.
fn for_each_node_mut(nodes: &mut [Value], f: &mut impl FnMut(&mut Value)) {
    for n in nodes.iter_mut() {
        f(n);
        if let Some(group) = n
            .get_mut("group")
            .and_then(|g| g.get_mut("nodes"))
            .and_then(|ns| ns.as_array_mut())
        {
            for_each_node_mut(group, f);
        }
    }
}

/// Whether the graph already carries a `world_to_mm` param on any
/// coc_from_depth node — the idempotence marker and the guard against
/// hand-customized graphs (a graph that already has the param is either
/// migrated or hand-authored; never touch it).
fn graph_has_world_to_mm(nodes: &[Value]) -> bool {
    flatten_nodes(nodes)
        .iter()
        .any(|n| {
            n.get("typeId").and_then(|t| t.as_str()) == Some(COC)
                && n.get("params")
                    .and_then(|p| p.get("world_to_mm"))
                    .is_some()
        })
}

/// Entry point wired into `crate::migrate::migrate_if_needed`'s 1.14.0 rung.
pub(crate) fn migrate(root: &mut Value) {
    let mut migrated = 0usize;
    crate::migrate::for_each_preset_instance(root, |fx| {
        let Value::Object(map) = fx else { return };
        if let Some(graph) = map.get_mut("graph") {
            migrated += usize::from(migrate_graph_value(graph));
        }
    });
    if let Some(presets) = root.get_mut("embeddedPresets").and_then(|v| v.as_array_mut()) {
        for preset in presets.iter_mut() {
            if let Some(def) = preset.get_mut("def") {
                migrated += usize::from(migrate_graph_value(def));
            }
        }
    }
    if migrated > 0 {
        super::note_migration(format!(
            "{migrated} scene graph(s) calibrated: CoC world_to_mm + f_stop scaled to scene radius (v1.14.0)"
        ));
    }
}

/// Migrate one graph-shaped value. Returns true when it was modified.
fn migrate_graph_value(graph: &mut Value) -> bool {
    let Value::Object(map) = graph else { return false };
    let Some(nodes) = map.get("nodes").and_then(|n| n.as_array()) else {
        return false;
    };
    // No coc_from_depth node → nothing to calibrate; byte-identical.
    if !flatten_nodes(nodes).iter().any(|n| n.get("typeId").and_then(|t| t.as_str()) == Some(COC)) {
        return false;
    }
    // Already migrated / hand-customized → skip (idempotent).
    if graph_has_world_to_mm(nodes) {
        return false;
    }
    // Graph carries no sceneBounds → world_to_mm stays at the 1000.0
    // default (old behavior); nothing to change.
    let Some(meta) = map.get("presetMetadata") else {
        return false;
    };
    let Some(radius) = scene_radius_from_meta(meta) else {
        return false;
    };

    let Value::Object(map) = graph else { return false };
    let Some(nodes) = map.get_mut("nodes").and_then(|n| n.as_array_mut()) else {
        return false;
    };

    // f_stop scaling pass FIRST (before world_to_mm is stamped, so the
    // value-shape check can't mistake the stamp for an f_stop): multiply
    // every stored f_stop param by the radius — the lens's value, wherever
    // the graph stores it (camera_lens node params, and any f_stop
    // port-shadow param on another node).
    for_each_node_mut(nodes, &mut |node| {
        let Some(Value::Object(params)) = node.get_mut("params") else { return };
        for (name, pv) in params.iter_mut() {
            if name != "f_stop" || pv.get("type").and_then(|t| t.as_str()) != Some("Float") {
                continue;
            }
            let Some(v) = pv.get("value").and_then(|v| v.as_f64()) else {
                continue;
            };
            pv["value"] = Value::from(v * f64::from(radius));
        }
    });
    // Stamp world_to_mm on every coc_from_depth node (there should be
    // exactly one in a DoF chain, but stamp all so none is left at 1000).
    let world_to_mm = (1000.0 / radius).min(100_000.0);
    for_each_node_mut(nodes, &mut |node| {
        if node.get("typeId").and_then(|t| t.as_str()) == Some(COC)
            && let Some(params) = node.get_mut("params").and_then(|p| p.as_object_mut())
        {
            params.insert(
                "world_to_mm".to_string(),
                serde_json::json!({"type": "Float", "value": world_to_mm}),
            );
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A graph JSON with an import shape: presetMetadata.sceneBounds, a
    /// camera_lens carrying f_stop, and a coc_from_depth node inside a group.
    fn scene_graph(scene_bounds: bool, f_stop: f64) -> Value {
        let mut meta = serde_json::json!({
            "id": "some_scene#1",
        });
        if scene_bounds {
            meta["sceneBounds"] = serde_json::json!([[-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]]);
        }
        serde_json::json!({
            "version": 2, "name": "Scene",
            "presetMetadata": meta,
            "nodes": [
                {"id": 1, "typeId": "node.orbit_camera", "nodeId": "cam", "params": {}},
                {"id": 2, "typeId": "node.camera_lens", "nodeId": "lens",
                 "params": {"f_stop": {"type": "Float", "value": f_stop}}},
                {"id": 3, "typeId": "group", "nodeId": "dof", "params": {},
                 "group": {"nodes": [
                    {"id": 4, "typeId": "node.coc_from_depth", "nodeId": "coc",
                     "params": {"max_radius": {"type": "Float", "value": 24.0}}}
                 ]}},
                {"id": 5, "typeId": "system.final_output", "nodeId": "final", "params": {}}
            ],
            "wires": []
        })
    }

    fn radius_of_bounds() -> f32 {
        let dims = [2.0f32; 3];
        (dims[0] * dims[0] + dims[1] * dims[1] + dims[2] * dims[2]).sqrt() * 0.5
    }

    fn coc_node_of(graph: &Value) -> &Value {
        flatten_nodes(graph.get("nodes").unwrap().as_array().unwrap())
            .iter()
            .find(|n| n.get("typeId").and_then(|t| t.as_str()) == Some(COC))
            .unwrap()
    }

    #[test]
    fn scene_bounds_graph_gets_world_to_mm_and_scaled_f_stop() {
        let graph = scene_graph(true, 2.8);
        // Apply via a full root (for_each_preset_instance wrapper allows it).
        let mut root = serde_json::json!({
            "projectVersion": "1.13.0",
            "timeline": {"layers": [{"genParams": {"graph": graph.clone()}}]}
        });
        migrate(&mut root);
        let migrated = &root["timeline"]["layers"][0]["genParams"]["graph"];

        let r = radius_of_bounds();
        let w2m = coc_node_of(migrated)["params"]["world_to_mm"].clone();
        assert_eq!(w2m["type"].as_str(), Some("Float"));
        assert!((w2m["value"].as_f64().unwrap() - 1000.0 / r as f64).abs() < 1e-3,
            "world_to_mm must be 1000/radius, got {}", w2m["value"]);

        // f_stop scaled by R (look-preserving).
        let lens = &migrated["nodes"][1];
        let stored = lens["params"]["f_stop"]["value"].as_f64().unwrap();
        assert!((stored - 2.8 * r as f64).abs() < 1e-3, "f_stop must be ×R, got {stored}");
    }

    #[test]
    fn graph_without_scene_bounds_is_untouched() {
        let graph = scene_graph(false, 2.8);
        let before = serde_json::to_string(&graph).unwrap();
        let mut root = serde_json::json!({
            "projectVersion": "1.13.0",
            "timeline": {"layers": [{"genParams": {"graph": graph}}]}
        });
        migrate(&mut root);
        let migrated = &root["timeline"]["layers"][0]["genParams"]["graph"];
        let after = serde_json::to_string(migrated).unwrap();
        assert_eq!(before, after, "no-sceneBounds graph must be byte-identical");
    }

    #[test]
    fn migration_is_idempotent() {
        let mut root = serde_json::json!({
            "projectVersion": "1.13.0",
            "timeline": {"layers": [{"genParams": {"graph": scene_graph(true, 2.8)}}]}
        });
        migrate(&mut root);
        let once = serde_json::to_string(&root).unwrap();
        migrate(&mut root);
        let twice = serde_json::to_string(&root).unwrap();
        assert_eq!(once, twice, "second migrate must be a no-op (idempotent)");
    }

    #[test]
    fn full_ladder_runs_the_world_to_mm_rung() {
        let root = serde_json::json!({
            "projectVersion": "1.13.0",
            "timeline": {"layers": [{"genParams": {"graph": scene_graph(true, 2.8)}}]}
        });
        let out = crate::migrate::migrate_if_needed(&serde_json::to_string(&root).unwrap()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["projectVersion"].as_str(),
            Some(manifold_core::project::CURRENT_PROJECT_VERSION),
            "ladder must march to CURRENT_PROJECT_VERSION"
        );
        let graph = &v["timeline"]["layers"][0]["genParams"]["graph"];
        assert!(
            coc_node_of(graph)["params"]["world_to_mm"].is_object(),
            "full ladder must stamp world_to_mm on the coc node"
        );
    }

    #[test]
    fn top_level_coc_node_without_group_is_migrated_too() {
        // CinematicScene-shaped: coc at top level, no dof group wrapper.
        let graph = serde_json::json!({
            "version": 2, "name": "Cinematic",
            "presetMetadata": {"id": "Cinematic#1", "sceneBounds": [[-1.0,-1.0,-1.0],[1.0,1.0,1.0]]},
            "nodes": [
                {"id": 1, "typeId": "node.orbit_camera", "nodeId": "cam", "params": {}},
                {"id": 2, "typeId": "node.camera_lens", "nodeId": "lens",
                 "params": {"f_stop": {"type": "Float", "value": 4.0}}},
                {"id": 3, "typeId": "node.coc_from_depth", "nodeId": "coc",
                 "params": {"max_radius": {"type": "Float", "value": 24.0}}},
                {"id": 4, "typeId": "node.bokeh_gather", "nodeId": "bokeh", "params": {}}
            ],
            "wires": []
        });
        let mut root = serde_json::json!({
            "projectVersion": "1.13.0",
            "embeddedPresets": [{"def": graph}]
        });
        migrate(&mut root);
        let def = &root["embeddedPresets"][0]["def"];
        let coc = &def["nodes"][2];
        assert!(coc["params"]["world_to_mm"].is_object(), "top-level coc must be stamped");
        let r = radius_of_bounds();
        let lens = &def["nodes"][1];
        assert!(
            (lens["params"]["f_stop"]["value"].as_f64().unwrap() - 4.0 * r as f64).abs() < 1e-3,
            "top-level coc graph's f_stop must scale by R"
        );
    }
}