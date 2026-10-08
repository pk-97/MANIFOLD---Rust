//! Host graphs that publish the liquid seam's face grid, for the face grid's
//! fusion check and its side-by-side demo: GPU FLIP through
//! `WaterScene::with_faces`, MPM through `WaterDamBreakMatter.json` with the
//! three components wired into its Live Matter group.

use manifold_core::effect_graph_def::EffectGraphDef;
use serde_json::{Value, json};

/// The matter components' node ids inside the Live Matter group, x, y and z.
pub(super) const MATTER_FACE_NODES: [&str; 3] = ["matter_face_u", "matter_face_v", "matter_face_w"];

/// The node between one axis's component and the frame in a `consumer` scene.
pub(super) const FACE_CONSUMER: &str = "matter_face_consumer";

/// The node giving the consumer its divisor: the length of the next axis's
/// face array.
pub(super) const FACE_DIVISOR: &str = "matter_face_length";

/// The 64-cell preset's u faces, 65 × 64 × 64: the divisor's row.
pub const DIVISOR_ROW: u32 = 65 * 64 * 64;

/// `WaterDamBreakMatter.json` with `node.matter_face_component` × 3 on
/// matter_state's grid feeding matter_frame's face inputs. `consumer` puts a
/// GPU per-element atom sized from its input between that axis's component
/// and the frame, so the two fold into one fused dispatch: node.divide_by_value,
/// chosen for its shape, not its meaning, dividing by the length of the u
/// faces (node.dot_products, a boundary). `consumer` is the v or w axis.
/// Without `collider` the moving box is unwired, so the tank holds only the
/// column and the pool, as GPU FLIP's Dam Break does.
pub fn matter_dam_break_faces(consumer: Option<usize>, collider: bool) -> EffectGraphDef {
    let json = crate::load::catalog_source::preset_json(&manifold_core::PresetTypeId::new("WaterDamBreakMatter"))
        .expect("registered catalog owner must provide WaterDamBreakMatter");
    let mut preset: Value = serde_json::from_str(&json).expect("preset parses");
    let nodes = preset["nodes"].as_array_mut().expect("preset nodes");
    let group = nodes.iter_mut().find(|n| n["nodeId"] == "Live Matter").expect("Live Matter group");
    let group_id = group["id"].clone();
    let inner = &mut group["group"];
    let id_of = |inner: &Value, name: &str| -> u64 {
        let nodes = inner["nodes"].as_array().expect("group nodes");
        nodes.iter().find(|n| n["nodeId"] == name).and_then(|n| n["id"].as_u64()).unwrap_or_else(|| panic!("no {name}"))
    };
    let (domain, state, frame) = (id_of(inner, "matter_domain"), id_of(inner, "matter_state"), id_of(inner, "matter_frame"));
    let next = inner["nodes"].as_array().expect("group nodes").iter().filter_map(|n| n["id"].as_u64()).max().expect("nodes") + 1;
    let mut wires = Vec::new();
    for (axis, name) in MATTER_FACE_NODES.into_iter().enumerate() {
        let id = next + axis as u64;
        // Decoy node counts: the domain's wired counts must win, fused or not.
        let decoy = json!({"type": "Int", "value": 8});
        inner["nodes"].as_array_mut().expect("group nodes").push(json!({
            "id": id, "typeId": "node.matter_face_component", "nodeId": name,
            "params": {"axis": {"type": "Enum", "value": axis}, "nodes_x": decoy, "nodes_y": decoy, "nodes_z": decoy},
        }));
        wires.push(json!({"fromNode": state, "fromPort": "grid", "toNode": id, "toPort": "grid"}));
        for port in ["nodes_x", "nodes_y", "nodes_z"] {
            wires.push(json!({"fromNode": domain, "fromPort": port, "toNode": id, "toPort": port}));
        }
        let to_frame = format!("face_{}_in", ["u", "v", "w"][axis]);
        if consumer == Some(axis) {
            assert!(axis > 0, "the u faces give the divisor");
            let (consumer, length) = (next + 3, next + 4);
            let int = |value: u32| json!({"type": "Int", "value": value});
            inner["nodes"].as_array_mut().expect("group nodes").extend([
                json!({"id": consumer, "typeId": "node.divide_by_value", "nodeId": FACE_CONSUMER, "params": {}}),
                json!({
                    "id": length, "typeId": "node.dot_products", "nodeId": FACE_DIVISOR,
                    "params": {"row_length": int(DIVISOR_ROW), "rows": int(1), "max_rows": int(1), "root": int(1)},
                }),
            ]);
            wires.push(json!({"fromNode": next, "fromPort": "out", "toNode": length, "toPort": "matrix"}));
            wires.push(json!({"fromNode": next, "fromPort": "out", "toNode": length, "toPort": "vector"}));
            wires.push(json!({"fromNode": id, "fromPort": "out", "toNode": consumer, "toPort": "values"}));
            wires.push(json!({"fromNode": length, "fromPort": "out", "toNode": consumer, "toPort": "divisor"}));
            wires.push(json!({"fromNode": consumer, "fromPort": "out", "toNode": frame, "toPort": to_frame}));
        } else {
            wires.push(json!({"fromNode": id, "fromPort": "out", "toNode": frame, "toPort": to_frame}));
        }
    }
    inner["wires"].as_array_mut().expect("group wires").extend(wires);
    if !collider {
        preset["wires"].as_array_mut().expect("preset wires").retain(|w| !(w["toNode"] == group_id && w["toPort"] == "collider"));
    }
    serde_json::from_value(preset).expect("matter faces def")
}
