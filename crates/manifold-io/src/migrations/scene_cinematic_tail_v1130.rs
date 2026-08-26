//! v1.12.0 → v1.13.0: inject the cinematic tail (`coc_from_depth →
//! `coc_dilate` → `bokeh_gather` → `motion_blur`) into every 3D scene graph
//! that predates it (`docs/CINEMATIC_SCENE_TAIL_DESIGN.md` D3).
//!
//! Pure `Value → Value`, pre-typed-deserialize (the quarantine rule — same
//! as `scene_transform_v1120`). Walks every graph-shaped JSON object in the
//! project: each preset instance's `graph` via the shared
//! `for_each_preset_instance` walk (which covers `timeline.layers[].genParams.graph`
//! — the convicted missing-chain location from the design's audit addendum:
//! the renderer builds the LAYER's graph, so layer graphs are the primary
//! target) AND every `embeddedPresets[].def`.
//!
//! A graph migrates iff ALL of these hold:
//!   - it has a TOP-LEVEL `node.render_scene` (imported-scene shape — see
//!     `gltf_import/scene.rs`, which emits object groups AND the shared
//!     render node side by side at top level);
//!   - NO node anywhere (top level or nested group body) has type
//!     `node.coc_from_depth` / `node.bokeh_gather` / `node.motion_blur` —
//!     the partial-tail rule: any chain member means the graph is already
//!     tailed or hand-customized (e.g. the pre-polish `variable_blur`
//!     chains), and touching it risks breaking a user's edits. Conservative
//!     skip, idempotent by construction;
//!   - a `system.final_output` sink exists at top level with exactly one
//!     wire into its `in` port. No sink or no such wire → skip WITH A
//!     LOAD-NOTE (the skip-loudly rule, surfaced in the "opened with
//!     repairs" toast via `LoadReport::migration_notes`) — never silently
//!     dropped, never a second insertion heuristic.
//!
//! Injection (flat, top level — the runtime topology of the import
//! assembler's `dof` group, without the editor group packaging):
//! the wire that fed `final.in` is re-anchored to `bokeh_gather.in`, then
//! `bokeh.out → motion_blur.in → final.in`; `render_scene.depth → coc.depth`,
//! `coc.out → coc_dilate.in → bokeh.width`, `render_scene.velocity →
//! `motion_blur.velocity`, and the lens feeds `coc.camera` +
//! `motion_blur.camera` so DoF and shutter read the SAME lens exposure
//! uses. A graph with no top-level `node.camera_lens` gains one — every
//! consumer of the camera wire feeding `render_scene.camera` is re-pointed
//! through it (that is what inserting a lens means) — with `f_stop` 1000
//! (DoF off until dialed), `shutter_angle` 180 (P4 amendment, Peter
//! 2026-08-26 night: motion smears by default; shutter 0 remains the exact
//! pass-through), `exposure_ev` 0, and `focus_distance` from the orbit
//! camera's `distance` param when present, else 10.0. Existing lens
//! nodes are reused as-is — their params are never touched.
//!
//! When the graph carries `presetMetadata` (every import-era graph does),
//! the migration also stamps the P4 Camera-section card entries —
//! motion_blur's `max_blur_px` + `enabled` and bokeh's `enabled` — matching
//! the import assembly's stamps (`gltf_import/scene.rs`), so migrated
//! projects get the same Scene Setup rows as fresh imports. A graph with no
//! `presetMetadata` has no card surface to extend and is left with the
//! bare tail.
//!
//! Idempotence: a migrated graph contains `node.coc_from_depth` +
//! `node.bokeh_gather` + `node.motion_blur`, so a second load's partial-tail
//! check skips it — asserted by `migration_is_idempotent`.

use std::collections::HashSet;

use serde_json::{Map, Value};

const RENDER_SCENE: &str = "node.render_scene";
const CAMERA_LENS: &str = "node.camera_lens";
const ORBIT_CAMERA: &str = "node.orbit_camera";
const COC: &str = "node.coc_from_depth";
const COC_DILATE: &str = "node.coc_dilate";
const BOKEH: &str = "node.bokeh_gather";
const MOTION_BLUR: &str = "node.motion_blur";
const FINAL_OUTPUT: &str = "system.final_output";
const GROUP: &str = "group";

/// Any of these anywhere in the graph = already tailed or hand-customized.
const TAIL_MARKER_TYPES: &[&str] = &[COC, BOKEH, MOTION_BLUR];

/// Entry point wired into `crate::migrate::migrate_if_needed`'s 1.13.0 rung.
pub(crate) fn migrate(root: &mut Value) {
    let mut injected = 0usize;
    crate::migrate::for_each_preset_instance(root, |fx| {
        let Value::Object(map) = fx else { return };
        if let Some(graph) = map.get_mut("graph") {
            injected += usize::from(migrate_graph_value(graph));
        }
    });
    if let Some(presets) = root.get_mut("embeddedPresets").and_then(|v| v.as_array_mut()) {
        for preset in presets.iter_mut() {
            if let Some(def) = preset.get_mut("def") {
                injected += usize::from(migrate_graph_value(def));
            }
        }
    }
    if injected > 0 {
        super::note_migration(format!(
            "{injected} 3D scene graph(s) upgraded: depth-of-field + motion blur chain added (v1.13.0)"
        ));
    }
}

/// Migrate one graph-shaped value (`nodes`/`wires`). Returns true when the
/// tail was injected.
fn migrate_graph_value(graph: &mut Value) -> bool {
    let Some(nodes) = graph.get("nodes").and_then(|n| n.as_array()) else {
        return false;
    };
    // Partial-tail / hand-customized check spans nested group bodies (an
    // old-chain group still means "has a chain").
    if graph_has_marker_recursive(nodes) {
        return false;
    }
    let Some(render) = nodes.iter().find(|n| n.get("typeId").and_then(|t| t.as_str()) == Some(RENDER_SCENE)) else {
        return false; // not a 3D scene graph — the common case, silent
    };
    let render_id = render.get("id").and_then(|i| i.as_u64()).unwrap_or(0) as u32;

    let final_node = nodes
        .iter()
        .find(|n| n.get("typeId").and_then(|t| t.as_str()) == Some(FINAL_OUTPUT));
    let Some(final_id) = final_node
        .and_then(|n| n.get("id"))
        .and_then(|i| i.as_u64())
        .map(|i| i as u32)
    else {
        skip_note("node.render_scene present but no system.final_output sink");
        return false;
    };
    let empty_wires = Vec::new();
    let wires = graph
        .get("wires")
        .and_then(|w| w.as_array())
        .unwrap_or(&empty_wires);
    let final_upstreams: Vec<(u32, String)> = wires
        .iter()
        .filter(|w| {
            w.get("toNode").and_then(|i| i.as_u64()) == Some(u64::from(final_id))
                && w.get("toPort").and_then(|p| p.as_str()) == Some("in")
        })
        .map(|w| {
            (
                w.get("fromNode").and_then(|i| i.as_u64()).unwrap_or(0) as u32,
                w.get("fromPort")
                    .and_then(|p| p.as_str())
                    .unwrap_or("out")
                    .to_string(),
            )
        })
        .collect();
    let [(up_node, up_port)] = final_upstreams.as_slice() else {
        skip_note("node.render_scene present but the final sink's `in` wire is absent or ambiguous");
        return false;
    };
    let (up_node, up_port) = (*up_node, up_port.clone());

    // Lens: reuse a top-level camera_lens, else insert one on the camera
    // wire feeding render_scene.camera (rewiring EVERY consumer of that
    // wire through the new lens).
    let mut ids = IdGen::from_graph(graph);
    let existing_lens = nodes
        .iter()
        .find(|n| n.get("typeId").and_then(|t| t.as_str()) == Some(CAMERA_LENS))
        .and_then(|n| n.get("id"))
        .and_then(|i| i.as_u64())
        .map(|i| i as u32);
    let mut extra_nodes: Vec<Value> = Vec::new();
    let mut rewires: Vec<(u32, u32)> = Vec::new(); // (wire index stays implicit; from_node old -> new)
    let mut lens_link: Option<Value> = None; // cam source -> new lens.camera (insert branch only)
    let lens_id = match existing_lens {
        Some(id) => id,
        None => {
            let cam_wire = wires.iter().find(|w| {
                w.get("toNode").and_then(|i| i.as_u64()) == Some(u64::from(render_id))
                    && w.get("toPort").and_then(|p| p.as_str()) == Some("camera")
            });
            let Some(cam_wire) = cam_wire else {
                skip_note("node.render_scene has no camera wire to insert a lens into");
                return false;
            };
            let cam_src = cam_wire.get("fromNode").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
            let cam_port = cam_wire
                .get("fromPort")
                .and_then(|p| p.as_str())
                .unwrap_or("out")
                .to_string();
            let focus = nodes
                .iter()
                .find(|n| {
                    n.get("id").and_then(|i| i.as_u64()) == Some(u64::from(cam_src))
                        && n.get("typeId").and_then(|t| t.as_str()) == Some(ORBIT_CAMERA)
                })
                .and_then(|c| c.get("params"))
                .and_then(|p| p.get("distance"))
                .and_then(|v| v.get("value"))
                .and_then(|v| v.as_f64())
                .unwrap_or(10.0);
            let lens_id = ids.next_id();
            let lens_node_id = ids.fresh_node_id("lens");
            extra_nodes.push(node_json(
                lens_id,
                &lens_node_id,
                CAMERA_LENS,
                &[
                    ("focus_distance", focus),
                    ("f_stop", 1000.0),
                    ("shutter_angle", 180.0),
                    ("exposure_ev", 0.0),
                ],
            ));
            // Every consumer of (cam_src.cam_port) reroutes through the lens.
            for (idx, w) in wires.iter().enumerate() {
                let from_matches = w.get("fromNode").and_then(|i| i.as_u64()) == Some(u64::from(cam_src))
                    && w.get("fromPort").and_then(|p| p.as_str()) == Some(cam_port.as_str());
                if from_matches {
                    rewires.push((idx as u32, lens_id));
                }
            }
            // ...and the camera source itself feeds the lens (the link the
            // re-pointing alone doesn't create).
            lens_link = Some(wire_json(cam_src, &cam_port, lens_id, "camera"));
            lens_id
        }
    };

    // Build the tail nodes.
    let coc_id = ids.next_id();
    let coc_node_id = ids.fresh_node_id("coc");
    let dilate_id = ids.next_id();
    let dilate_node_id = ids.fresh_node_id("coc_dilate");
    let bokeh_id = ids.next_id();
    let bokeh_node_id = ids.fresh_node_id("bokeh");
    let mb_id = ids.next_id();
    let mb_node_id = ids.fresh_node_id("motion_blur");
    extra_nodes.push(node_json(coc_id, &coc_node_id, COC, &[("max_radius", 24.0)]));
    extra_nodes.push(node_json(dilate_id, &dilate_node_id, COC_DILATE, &[]));
    extra_nodes.push(node_json(bokeh_id, &bokeh_node_id, BOKEH, &[("max_radius", 24.0)]));
    extra_nodes.push(node_json(mb_id, &mb_node_id, MOTION_BLUR, &[("max_blur_px", 32.0)]));

    // Rewire: previous final-upstream -> bokeh.in; drop the old ->final.in wire.
    let mut new_wires: Vec<Value> = Vec::new();
    new_wires.extend(lens_link);
    new_wires.push(wire_json(up_node, &up_port, bokeh_id, "in"));
    new_wires.push(wire_json(render_id, "depth", coc_id, "depth"));
    new_wires.push(wire_json(lens_id, "out", coc_id, "camera"));
    new_wires.push(wire_json(coc_id, "out", dilate_id, "in"));
    new_wires.push(wire_json(dilate_id, "out", bokeh_id, "width"));
    new_wires.push(wire_json(bokeh_id, "out", mb_id, "in"));
    new_wires.push(wire_json(render_id, "velocity", mb_id, "velocity"));
    new_wires.push(wire_json(lens_id, "out", mb_id, "camera"));
    new_wires.push(wire_json(mb_id, "out", final_id, "in"));

    let Value::Object(map) = graph else { return false };
    let Some(nodes_arr) = map.get_mut("nodes").and_then(|n| n.as_array_mut()) else {
        return false;
    };
    nodes_arr.extend(extra_nodes);
    let Some(wires_arr) = map.get_mut("wires").and_then(|w| w.as_array_mut()) else {
        return false;
    };
    // Apply the lens re-pointing FIRST — its indices reference the
    // pre-retain array; then drop the old ->final.in wire; then extend.
    for (idx, new_from) in &rewires {
        if let Some(w) = wires_arr.get_mut(*idx as usize) {
            w["fromNode"] = Value::from(*new_from);
        }
    }
    wires_arr.retain(|w| {
        !(w.get("toNode").and_then(|i| i.as_u64()) == Some(u64::from(final_id))
            && w.get("toPort").and_then(|p| p.as_str()) == Some("in"))
    });
    wires_arr.extend(new_wires);
    stamp_tail_metadata(map, mb_id, &mb_node_id, bokeh_id, &bokeh_node_id);
    true
}

/// P4: append the Camera-section card entries for the injected tail
/// (motion_blur `max_blur_px` + `enabled`, bokeh `enabled`) to the graph's
/// `presetMetadata`, mirroring the import assembly's stamps
/// (`scene_exposure::stamp_scene_node_exposures_into` — same id shape
/// (`{doc_id}_{param}`), same `defaultMirrorsNodeParam`, same
/// `cardVisible: false` default-deny the lens rows already use; the Scene
/// Setup panel reads section metadata independently of that flag). No-op
/// when the graph has no `presetMetadata` (nothing to extend) or when a
/// binding for the same target already exists.
fn stamp_tail_metadata(
    map: &mut Map<String, Value>,
    mb_id: u32,
    mb_node_id: &str,
    bokeh_id: u32,
    bokeh_node_id: &str,
) {
    let Some(meta) = map.get_mut("presetMetadata").and_then(|m| m.as_object_mut()) else {
        return;
    };
    // Disjoint mutable borrows of the two arrays via one iter_mut pass.
    let (mut params, mut bindings) = (None, None);
    for (k, v) in meta.iter_mut() {
        match k.as_str() {
            "params" => params = v.as_array_mut(),
            "bindings" => bindings = v.as_array_mut(),
            _ => {}
        }
    }
    let (Some(params), Some(bindings)) = (params, bindings) else {
        return;
    };

    // (doc_id, node_id, param, label, min, max, default, is_toggle)
    type StampEntry<'a> = (u32, &'a str, &'a str, &'a str, f64, f64, f64, bool);
    let entries: [StampEntry<'_>; 3] = [
        (mb_id, mb_node_id, "max_blur_px", "Max Blur (px)", 0.0, 128.0, 32.0, false),
        (mb_id, mb_node_id, "enabled", "Enabled", 0.0, 1.0, 1.0, true),
        (bokeh_id, bokeh_node_id, "enabled", "Enabled", 0.0, 1.0, 1.0, true),
    ];
    for (doc_id, node_id, param, label, min, max, default, is_toggle) in entries {
        let already = bindings.iter().any(|b| {
            b.get("target").and_then(|t| t.get("nodeId")).and_then(|n| n.as_str()) == Some(node_id)
                && b.get("target").and_then(|t| t.get("param")).and_then(|p| p.as_str())
                    == Some(param)
        });
        if already {
            continue;
        }
        let id = format!("{doc_id}_{param}");
        let mut spec = serde_json::json!({
            "id": id,
            "name": label,
            "min": min,
            "max": max,
            "defaultValue": default,
            "section": "Camera",
            "cardVisible": false,
        });
        if is_toggle {
            spec["isToggle"] = Value::from(true);
        }
        params.push(spec);
        bindings.push(serde_json::json!({
            "id": id,
            "label": label,
            "defaultValue": default,
            "target": {"kind": "node", "nodeId": node_id, "param": param},
            "defaultMirrorsNodeParam": true,
        }));
    }
}

fn skip_note(reason: &str) {
    super::note_migration(format!(
        "cinematic-tail migration skipped one scene graph: {reason} — left untouched"
    ));
}

fn graph_has_marker_recursive(nodes: &[Value]) -> bool {
    nodes.iter().any(|n| {
        let ty = n.get("typeId").and_then(|t| t.as_str());
        if TAIL_MARKER_TYPES.contains(&ty.unwrap_or("")) {
            return true;
        }
        if ty == Some(GROUP)
            && let Some(inner) = n.get("group").and_then(|g| g.get("nodes")).and_then(|ns| ns.as_array())
        {
            return graph_has_marker_recursive(inner);
        }
        false
    })
}

fn node_json(id: u32, node_id: &str, type_id: &str, params: &[(&str, f64)]) -> Value {
    let mut p = Map::new();
    for (name, value) in params {
        p.insert(
            (*name).to_string(),
            serde_json::json!({"type": "Float", "value": value}),
        );
    }
    serde_json::json!({
        "id": id,
        "typeId": type_id,
        "nodeId": node_id,
        "params": Value::Object(p),
    })
}

fn wire_json(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> Value {
    serde_json::json!({
        "fromNode": from_node,
        "fromPort": from_port,
        "toNode": to_node,
        "toPort": to_port,
    })
}

/// Monotonic id/nodeId minter seeded from a whole-graph scan (including
/// nested group bodies — the id namespace is global across the document,
/// per `scene_transform_v1120`'s IdGen).
struct IdGen {
    next_id: u32,
    used_node_ids: HashSet<String>,
}

impl IdGen {
    fn from_graph(graph: &Value) -> Self {
        let mut max_id = 0u32;
        let mut used = HashSet::new();
        fn scan(nodes: &[Value], max_id: &mut u32, used: &mut HashSet<String>) {
            for n in nodes {
                if let Some(i) = n.get("id").and_then(|i| i.as_u64()) {
                    *max_id = (*max_id).max(i as u32);
                }
                if let Some(s) = n.get("nodeId").and_then(|s| s.as_str()) {
                    used.insert(s.to_string());
                }
                if n.get("typeId").and_then(|t| t.as_str()) == Some(GROUP)
                    && let Some(inner) =
                        n.get("group").and_then(|g| g.get("nodes")).and_then(|ns| ns.as_array())
                {
                    scan(inner, max_id, used);
                }
            }
        }
        if let Some(nodes) = graph.get("nodes").and_then(|n| n.as_array()) {
            scan(nodes, &mut max_id, &mut used);
        }
        IdGen { next_id: max_id + 1, used_node_ids: used }
    }

    fn next_id(&mut self) -> u32 {
        let v = self.next_id;
        self.next_id += 1;
        v
    }

    fn fresh_node_id(&mut self, base: &str) -> String {
        if self.used_node_ids.insert(base.to_string()) {
            return base.to_string();
        }
        for k in 2u32.. {
            let candidate = format!("{base}_{k}");
            if self.used_node_ids.insert(candidate.clone()) {
                return candidate;
            }
        }
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal project: one generator layer whose genParams.graph is a
    /// pre-tail 3D scene (orbit camera -> render_scene -> final), plus one
    /// embedded preset with the same shape, and one graph with an OLD-chain
    /// marker (motion_blur) that must be skipped.
    fn fixture() -> Value {
        serde_json::json!({
            "projectVersion": "1.12.0",
            "timeline": {"layers": [{
                "name": "scene",
                "genParams": {
                    "generatorType": "some_scene#1",
                    "graph": {
                        "version": 2, "name": "Scene",
                        "nodes": [
                            {"id": 1, "typeId": "node.orbit_camera", "nodeId": "cam", "params": {"distance": {"type": "Float", "value": 7.5}}},
                            {"id": 2, "typeId": "node.render_scene", "nodeId": "scene", "params": {}},
                            {"id": 3, "typeId": "system.final_output", "nodeId": "final", "params": {}}
                        ],
                        "wires": [
                            {"fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "camera"},
                            {"fromNode": 2, "fromPort": "color", "toNode": 3, "toPort": "in"}
                        ]
                    }
                }
            }]},
            "embeddedPresets": [{
                "name": "emb",
                "def": {
                    "version": 2, "name": "Emb",
                    "nodes": [
                        {"id": 1, "typeId": "node.orbit_camera", "nodeId": "cam", "params": {}},
                        {"id": 4, "typeId": "node.camera_lens", "nodeId": "lens", "params": {"f_stop": {"type": "Float", "value": 2.8}}},
                        {"id": 2, "typeId": "node.render_scene", "nodeId": "scene", "params": {}},
                        {"id": 3, "typeId": "system.final_output", "nodeId": "final", "params": {}}
                    ],
                    "wires": [
                        {"fromNode": 1, "fromPort": "out", "toNode": 4, "toPort": "camera"},
                        {"fromNode": 4, "fromPort": "out", "toNode": 2, "toPort": "camera"},
                        {"fromNode": 2, "fromPort": "color", "toNode": 3, "toPort": "in"}
                    ]
                }
            }]
        })
    }

    fn migrated_fixture() -> Value {
        let json = serde_json::to_string(&fixture()).unwrap();
        let out = crate::migrate::migrate_if_needed(&json).unwrap();
        serde_json::from_str(&out).unwrap()
    }

    fn graph_nodes(graph: &Value) -> &Vec<Value> {
        graph.get("nodes").and_then(|n| n.as_array()).unwrap()
    }

    fn graph_wires(graph: &Value) -> &Vec<Value> {
        graph.get("wires").and_then(|w| w.as_array()).unwrap()
    }

    fn has_type(graph: &Value, type_id: &str) -> bool {
        graph_nodes(graph)
            .iter()
            .any(|n| n.get("typeId").and_then(|t| t.as_str()) == Some(type_id))
    }

    fn has_wire(graph: &Value, from_type: &str, from_port: &str, to_type: &str, to_port: &str) -> bool {
        let id_of = |ty: &str| {
            graph_nodes(graph)
                .iter()
                .find(|n| n.get("typeId").and_then(|t| t.as_str()) == Some(ty))
                .and_then(|n| n.get("id"))
                .and_then(|i| i.as_u64())
        };
        let (Some(f), Some(t)) = (id_of(from_type), id_of(to_type)) else {
            return false;
        };
        graph_wires(graph).iter().any(|w| {
            w.get("fromNode").and_then(|i| i.as_u64()) == Some(f)
                && w.get("fromPort").and_then(|p| p.as_str()) == Some(from_port)
                && w.get("toNode").and_then(|i| i.as_u64()) == Some(t)
                && w.get("toPort").and_then(|p| p.as_str()) == Some(to_port)
        })
    }

    fn layer_graph(v: &Value) -> &Value {
        &v["timeline"]["layers"][0]["genParams"]["graph"]
    }

    #[test]
    fn layer_graph_gains_full_tail_with_inserted_lens() {
        let v = migrated_fixture();
        assert_eq!(v["projectVersion"].as_str(), Some("1.13.0"));
        let g = layer_graph(&v);
        for ty in [COC, COC_DILATE, BOKEH, MOTION_BLUR, CAMERA_LENS] {
            assert!(has_type(g, ty), "migrated layer graph must contain {ty}");
        }
        // Chain wiring: render.color -> bokeh.in; bokeh -> motion_blur -> final.
        assert!(has_wire(g, RENDER_SCENE, "color", BOKEH, "in"));
        assert!(has_wire(g, BOKEH, "out", MOTION_BLUR, "in"));
        assert!(has_wire(g, MOTION_BLUR, "out", FINAL_OUTPUT, "in"));
        assert!(has_wire(g, RENDER_SCENE, "depth", COC, "depth"));
        assert!(has_wire(g, COC, "out", COC_DILATE, "in"));
        assert!(has_wire(g, COC_DILATE, "out", BOKEH, "width"));
        assert!(has_wire(g, RENDER_SCENE, "velocity", MOTION_BLUR, "velocity"));
        assert!(has_wire(g, CAMERA_LENS, "out", COC, "camera"));
        assert!(has_wire(g, CAMERA_LENS, "out", MOTION_BLUR, "camera"));
        // The camera wire now routes through the inserted lens.
        assert!(has_wire(g, ORBIT_CAMERA, "out", CAMERA_LENS, "camera"));
        assert!(has_wire(g, CAMERA_LENS, "out", RENDER_SCENE, "camera"));
        // No direct color->final wire survives.
        assert!(!has_wire(g, RENDER_SCENE, "color", FINAL_OUTPUT, "in"));
        // Inserted lens: f_stop neutral, shutter 180 (P4), focus from the
        // orbit distance.
        let lens = graph_nodes(g)
            .iter()
            .find(|n| n.get("typeId").and_then(|t| t.as_str()) == Some(CAMERA_LENS))
            .unwrap();
        let p = &lens["params"];
        assert_eq!(p["f_stop"]["value"].as_f64(), Some(1000.0));
        assert_eq!(p["shutter_angle"]["value"].as_f64(), Some(180.0));
        assert_eq!(p["focus_distance"]["value"].as_f64(), Some(7.5));
    }

    #[test]
    fn migrated_graph_with_preset_metadata_gains_camera_section_stamps() {
        let mut f = fixture();
        {
            let g = &mut f["timeline"]["layers"][0]["genParams"]["graph"];
            g["presetMetadata"] = serde_json::json!({
                "id": "some_scene", "displayName": "Some Scene",
                "category": "Spatial", "oscPrefix": "some_scene",
                "params": [], "bindings": []
            });
        }
        let out = crate::migrate::migrate_if_needed(&serde_json::to_string(&f).unwrap()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let g = layer_graph(&v);
        let meta = &g["presetMetadata"];
        let params = meta["params"].as_array().unwrap();
        let bindings = meta["bindings"].as_array().unwrap();
        assert_eq!(params.len(), 3, "max_blur_px + 2 enabled toggles, got {params:?}");
        assert_eq!(bindings.len(), 3);
        let has = |node_id: &str, param: &str| {
            bindings.iter().any(|b| {
                b["target"]["nodeId"].as_str() == Some(node_id)
                    && b["target"]["param"].as_str() == Some(param)
            })
        };
        assert!(has("motion_blur", "max_blur_px"));
        assert!(has("motion_blur", "enabled"));
        assert!(has("bokeh", "enabled"));
        for p in params {
            assert_eq!(p["section"].as_str(), Some("Camera"));
        }
        let toggle = params.iter().find(|p| p["id"].as_str().unwrap().ends_with("_enabled")).unwrap();
        assert_eq!(toggle["isToggle"].as_bool(), Some(true));
        assert_eq!(toggle["defaultValue"].as_f64(), Some(1.0));
        // Idempotence of the stamp itself: a second migration pass over the
        // already-tailed graph is skipped by the marker check, and even a
        // direct re-stamp finds the targets present.
        let twice = crate::migrate::migrate_if_needed(&serde_json::to_string(&v).unwrap()).unwrap();
        let v2: Value = serde_json::from_str(&twice).unwrap();
        let meta2 = &layer_graph(&v2)["presetMetadata"];
        assert_eq!(meta2["params"].as_array().unwrap().len(), 3);
        assert_eq!(meta2["bindings"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn embedded_preset_reuses_existing_lens_untouched() {
        let v = migrated_fixture();
        let g = &v["embeddedPresets"][0]["def"];
        let lenses: Vec<&Value> = graph_nodes(g)
            .iter()
            .filter(|n| n.get("typeId").and_then(|t| t.as_str()) == Some(CAMERA_LENS))
            .collect();
        assert_eq!(lenses.len(), 1, "exactly one lens — the pre-existing one, reused");
        // Its f_stop (2.8, a user value) is never rewritten.
        assert_eq!(lenses[0]["params"]["f_stop"]["value"].as_f64(), Some(2.8));
        assert!(has_wire(g, CAMERA_LENS, "out", COC, "camera"));
        assert!(has_wire(g, MOTION_BLUR, "out", FINAL_OUTPUT, "in"));
    }

    #[test]
    fn migration_is_idempotent() {
        let once = migrated_fixture();
        let twice = crate::migrate::migrate_if_needed(&serde_json::to_string(&once).unwrap()).unwrap();
        let twice_v: Value = serde_json::from_str(&twice).unwrap();
        assert_eq!(
            serde_json::to_string(&once).unwrap(),
            serde_json::to_string(&twice_v).unwrap(),
            "second load must be a byte-exact no-op"
        );
    }

    #[test]
    fn graph_with_existing_chain_marker_is_skipped() {
        let mut f = fixture();
        // Add an old-chain motion_blur to the layer graph.
        let g = &mut f["timeline"]["layers"][0]["genParams"]["graph"];
        g["nodes"].as_array_mut().unwrap().push(
            serde_json::json!({"id": 9, "typeId": "node.motion_blur", "nodeId": "mb", "params": {}}),
        );
        let out = crate::migrate::migrate_if_needed(&serde_json::to_string(&f).unwrap()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let g = layer_graph(&v);
        assert!(!has_type(g, BOKEH), "old-chain graphs are left for Peter — never rewritten");
        assert!(!has_type(g, COC));
    }

    #[test]
    fn missing_final_sink_skips_loudly() {
        let mut f = fixture();
        {
            let g = &mut f["timeline"]["layers"][0]["genParams"]["graph"];
            g["nodes"].as_array_mut().unwrap().retain(|n| {
                n.get("typeId").and_then(|t| t.as_str()) != Some(FINAL_OUTPUT)
            });
        }
        let _ = crate::migrations::take_migration_notes(); // drain prior
        let out = crate::migrate::migrate_if_needed(&serde_json::to_string(&f).unwrap()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(
            !has_type(layer_graph(&v), BOKEH),
            "no final sink -> untouched"
        );
        let notes = crate::migrations::take_migration_notes();
        assert!(
            notes.iter().any(|n| n.contains("skipped") && n.contains("final")),
            "skip must leave a load note, got: {notes:?}"
        );
    }

    #[test]
    fn user_lens_edit_survives_a_second_save_reload() {
        // Round-trip rule: migrate, then a user edit (f_stop on the inserted
        // lens) persists through another save/load cycle with the tail intact.
        let mut v = migrated_fixture();
        {
            let g = &mut v["timeline"]["layers"][0]["genParams"]["graph"];
            let lens = g["nodes"].as_array_mut().unwrap().iter_mut().find(|n| {
                n.get("typeId").and_then(|t| t.as_str()) == Some(CAMERA_LENS)
            }).unwrap();
            lens["params"]["f_stop"]["value"] = Value::from(2.0);
        }
        let out = crate::migrate::migrate_if_needed(&serde_json::to_string(&v).unwrap()).unwrap();
        let v2: Value = serde_json::from_str(&out).unwrap();
        let g = layer_graph(&v2);
        let lens = graph_nodes(g)
            .iter()
            .find(|n| n.get("typeId").and_then(|t| t.as_str()) == Some(CAMERA_LENS))
            .unwrap();
        assert_eq!(lens["params"]["f_stop"]["value"].as_f64(), Some(2.0));
        assert!(has_type(g, MOTION_BLUR));
    }
}
