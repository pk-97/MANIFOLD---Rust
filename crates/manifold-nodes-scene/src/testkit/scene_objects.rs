//! Proof graphs written before the object port wire each object's parts
//! straight into `node.render_scene` (`mesh_i`, `transform_i`, `material_i`,
//! `instances_i`, the `*_map_i` textures). The renderer takes one `object_i`
//! port per object; [`bundle_scene_objects`] gives each object a
//! `node.scene_object` carrying those parts, as the scene compile wires one.

use serde_json::{json, Value};

/// `graph` with every renderer's per-object part wires moved onto one
/// `node.scene_object` per object, wired into its `object_i` port.
pub fn bundle_scene_objects(graph: &str) -> String {
    let mut def: Value = serde_json::from_str(graph).expect("a proof graph is JSON");
    let nodes = def["nodes"].as_array().expect("a proof graph has nodes");
    let renderers: Vec<u64> = nodes
        .iter()
        .filter(|node| node["typeId"] == "node.render_scene")
        .filter_map(|node| node["id"].as_u64())
        .collect();
    let mut next = nodes.iter().filter_map(|node| node["id"].as_u64()).max().unwrap_or(0) + 1;
    let mut objects: Vec<((u64, u64), u64)> = Vec::new();
    let (mut added_nodes, mut added_wires) = (Vec::new(), Vec::new());
    for wire in def["wires"].as_array_mut().expect("a proof graph has wires") {
        let Some(renderer) = wire["toNode"].as_u64().filter(|to| renderers.contains(to)) else { continue };
        let Some((part, index)) = wire["toPort"].as_str().and_then(object_part) else { continue };
        let object = match objects.iter().find(|(key, _)| *key == (renderer, index)) {
            Some(&(_, object)) => object,
            None => {
                let object = next;
                next += 1;
                objects.push(((renderer, index), object));
                added_nodes.push(json!({
                    "id": object, "typeId": "node.scene_object", "nodeId": format!("object_{renderer}_{index}"), "params": {}
                }));
                added_wires.push(json!({
                    "fromNode": object, "fromPort": "object", "toNode": renderer, "toPort": format!("object_{index}")
                }));
                object
            }
        };
        wire["toNode"] = json!(object);
        wire["toPort"] = json!(part);
    }
    def["nodes"].as_array_mut().expect("nodes").extend(added_nodes);
    def["wires"].as_array_mut().expect("wires").extend(added_wires);
    def.to_string()
}

/// The scene object input an old renderer port fed, and the object's index.
fn object_part(port: &str) -> Option<(String, u64)> {
    let (family, index) = port.rsplit_once('_')?;
    let index = index.parse().ok()?;
    let part = match family {
        "mesh" => "vertices",
        "transform" | "material" | "instances" | "indices" => family,
        map if map.ends_with("_map") => map,
        _ => return None,
    };
    Some((part.to_owned(), index))
}
