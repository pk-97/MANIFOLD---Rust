//! `scene add-camera-tail`: give a hand-authored 3D scene graph the same lens,
//! depth-of-field and motion-blur chain a GLB import gets
//! (`gltf_import/scene.rs` + `gltf_import/cinematic_tail.rs` are the template).
//!
//! The chain goes between `render_scene.color` and whatever consumed it, so a
//! tone map stays last and blur runs on HDR color, as in the importer where
//! nothing sits after motion blur. Lens and bokeh rows on the Camera card are
//! stamped at load by `migrate_scene_exposures` (both are scene vocabulary);
//! motion blur is not vocabulary, so its Max Blur row is stamped here,
//! matching the importer's id and label. No Motion Blur on/off row: placed
//! before a tone map, motion blur always fuses, and a fused member's `enabled`
//! breaks the load and is never read (BUG-3xdec (fusion ignores Bool on/off
//! params)). Shutter 0 is the exact off. Stamp the toggle once that is fixed.

use serde_json::{Value, json};

const RENDER_SCENE: &str = "node.render_scene";
const CAMERA_LENS: &str = "node.camera_lens";
const ORBIT_CAMERA: &str = "node.orbit_camera";
const TAIL_MARKERS: &[&str] = &["node.coc_from_depth", "node.bokeh_gather", "node.motion_blur"];

pub(super) fn add_camera_tail(graph: &mut Value) -> Result<String, String> {
    let nodes = graph
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or("layer graph has no nodes array")?;
    if has_marker(nodes) {
        return Err("graph already has a depth-of-field or motion-blur node; left untouched".into());
    }
    let renders: Vec<u64> = nodes
        .iter()
        .filter(|n| type_of(n) == Some(RENDER_SCENE))
        .filter_map(|n| n.get("id").and_then(Value::as_u64))
        .collect();
    let [render_id] = renders.as_slice() else {
        return Err(format!("expected exactly one top-level node.render_scene, found {}", renders.len()));
    };
    let render_id = *render_id;
    let wires = graph.get("wires").and_then(Value::as_array).cloned().unwrap_or_default();

    let cam_wire = wires
        .iter()
        .find(|w| to_is(w, render_id, "camera"))
        .ok_or("node.render_scene has no camera wire")?;
    let cam_src = cam_wire["fromNode"].as_u64().unwrap_or(0);
    let cam_port = cam_wire["fromPort"].as_str().unwrap_or("out").to_string();
    let cam_src_type = nodes.iter().find(|n| id_of(n) == Some(cam_src)).and_then(type_of);

    let mut next = max_id(nodes) + 1;
    let mut fresh = || {
        let id = next;
        next += 1;
        id
    };
    let mut new_nodes = Vec::new();
    let mut new_wires = Vec::new();
    let mut wires = wires;

    let lens_id = if cam_src_type == Some(CAMERA_LENS) {
        cam_src
    } else {
        // Importer seed: focus at the orbit distance, f/32, 180° shutter.
        let focus = nodes
            .iter()
            .find(|n| id_of(n) == Some(cam_src) && type_of(n) == Some(ORBIT_CAMERA))
            .and_then(|c| c.pointer("/params/distance/value"))
            .and_then(Value::as_f64)
            .unwrap_or(10.0);
        let lens_id = fresh();
        new_nodes.push(node(lens_id, "lens", CAMERA_LENS, json!({
            "focus_distance": {"type": "Float", "value": focus},
            "f_stop": {"type": "Float", "value": 32.0},
            "shutter_angle": {"type": "Float", "value": 180.0},
        })));
        // Every consumer of the raw camera now reads it through the lens.
        for w in wires.iter_mut() {
            if w["fromNode"].as_u64() == Some(cam_src) && w["fromPort"].as_str() == Some(cam_port.as_str()) {
                w["fromNode"] = json!(lens_id);
                w["fromPort"] = json!("out");
            }
        }
        new_wires.push(wire(cam_src, &cam_port, lens_id, "camera"));
        lens_id
    };

    let (dof_id, bokeh_id, dof_group) = dof_group(&mut fresh);
    new_nodes.push(dof_group);
    let mb_id = fresh();
    new_nodes.push(node(mb_id, "motion_blur", "node.motion_blur", json!({
        "max_blur_px": {"type": "Float", "value": 32.0},
    })));

    // Whatever read the scene's color now reads the blurred color.
    let mut color_consumers = 0;
    for w in wires.iter_mut() {
        if w["fromNode"].as_u64() == Some(render_id) && w["fromPort"].as_str() == Some("color") {
            w["fromNode"] = json!(mb_id);
            w["fromPort"] = json!("out");
            color_consumers += 1;
        }
    }
    if color_consumers == 0 {
        return Err("node.render_scene color output is not wired to anything".into());
    }
    new_wires.extend([
        wire(render_id, "color", dof_id, "color"),
        wire(render_id, "depth", dof_id, "depth"),
        wire(lens_id, "out", dof_id, "camera"),
        wire(dof_id, "out", mb_id, "in"),
        wire(render_id, "velocity", mb_id, "velocity"),
        wire(lens_id, "out", mb_id, "camera"),
    ]);
    wires.extend(new_wires);

    let map = graph.as_object_mut().ok_or("layer graph is not an object")?;
    map["nodes"].as_array_mut().ok_or("nodes")?.extend(new_nodes);
    map.insert("wires".into(), Value::Array(wires));
    stamp_motion_blur_rows(map, mb_id);
    Ok(format!(
        "added lens #{lens_id}, Depth of Field group #{dof_id} (bokeh #{bokeh_id}), motion blur #{mb_id}; {color_consumers} color consumer(s) rerouted"
    ))
}

/// The importer's `dof` group: `coc_from_depth → bokeh_gather`, bokeh on.
fn dof_group(fresh: &mut impl FnMut() -> u64) -> (u64, u64, Value) {
    let (in_id, coc_id, bokeh_id, out_id, group_id) = (fresh(), fresh(), fresh(), fresh(), fresh());
    let body_nodes = vec![
        node(in_id, "dof_in", "system.group_input", json!({})),
        node(coc_id, "coc", "node.coc_from_depth", json!({
            "max_radius": {"type": "Float", "value": 24.0},
        })),
        node(bokeh_id, "bokeh", "node.bokeh_gather", json!({
            "max_radius": {"type": "Float", "value": 24.0},
            "enabled": {"type": "Bool", "value": true},
            "aperture": {"type": "Enum", "value": 0},
            "quality": {"type": "Enum", "value": 1},
            "blur_alpha": {"type": "Bool", "value": true},
        })),
        node(out_id, "dof_out", "system.group_output", json!({})),
    ];
    let body_wires = vec![
        wire(in_id, "depth", coc_id, "depth"),
        wire(in_id, "camera", coc_id, "camera"),
        wire(coc_id, "out", bokeh_id, "width"),
        wire(in_id, "color", bokeh_id, "in"),
        wire(bokeh_id, "out", out_id, "out"),
    ];
    let mut group = node(group_id, "dof", "group", json!({}));
    group["title"] = json!("Depth of Field");
    group["group"] = json!({
        "interface": {
            "inputs": [
                {"name": "depth", "portType": "Texture2D"},
                {"name": "camera", "portType": "Camera"},
                {"name": "color", "portType": "Texture2D"},
            ],
            "outputs": [{"name": "out", "portType": "Texture2D"}],
        },
        "nodes": body_nodes,
        "wires": body_wires,
    });
    (group_id, bokeh_id, group)
}

/// Same id and label as the importer's motion-blur Max Blur row.
fn stamp_motion_blur_rows(map: &mut serde_json::Map<String, Value>, mb_id: u64) {
    let Some(meta) = map.get_mut("presetMetadata").and_then(Value::as_object_mut) else {
        return;
    };
    let id = format!("{mb_id}_max_blur_px");
    let spec = json!({
        "id": id, "name": "Max Blur (px)", "min": 0.0, "max": 128.0, "defaultValue": 32.0,
        "section": "Camera", "cardVisible": false,
    });
    let binding = json!({
        "id": id, "label": "Max Blur (px)", "defaultValue": 32.0,
        "target": {"kind": "node", "nodeId": "motion_blur", "param": "max_blur_px"},
        "defaultMirrorsNodeParam": true,
    });
    if let Some(params) = meta.get_mut("params").and_then(Value::as_array_mut) {
        params.push(spec);
    }
    if let Some(bindings) = meta.get_mut("bindings").and_then(Value::as_array_mut) {
        bindings.push(binding);
    }
}

fn node(id: u64, node_id: &str, type_id: &str, params: Value) -> Value {
    json!({"id": id, "nodeId": node_id, "typeId": type_id, "handle": node_id, "params": params})
}

fn wire(from: u64, from_port: &str, to: u64, to_port: &str) -> Value {
    json!({"fromNode": from, "fromPort": from_port, "toNode": to, "toPort": to_port})
}

fn type_of(n: &Value) -> Option<&str> {
    n.get("typeId").and_then(Value::as_str)
}

fn id_of(n: &Value) -> Option<u64> {
    n.get("id").and_then(Value::as_u64)
}

fn to_is(w: &Value, node: u64, port: &str) -> bool {
    w["toNode"].as_u64() == Some(node) && w["toPort"].as_str() == Some(port)
}

fn has_marker(nodes: &[Value]) -> bool {
    nodes.iter().any(|n| {
        type_of(n).is_some_and(|t| TAIL_MARKERS.contains(&t))
            || n.pointer("/group/nodes").and_then(Value::as_array).is_some_and(|b| has_marker(b))
    })
}

/// Ids are unique across the whole def, group bodies included.
fn max_id(nodes: &[Value]) -> u64 {
    nodes
        .iter()
        .map(|n| {
            let own = id_of(n).unwrap_or(0);
            let inner = n.pointer("/group/nodes").and_then(Value::as_array).map_or(0, |b| max_id(b));
            own.max(inner)
        })
        .max()
        .unwrap_or(0)
}
