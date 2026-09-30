//! Host graphs that publish the liquid seam's face grid, for the face grid's
//! fusion check and its side-by-side demo: SWASH through
//! `WaterScene::with_faces`, MPM through `WaterDamBreakMatter.json` with the
//! three components wired into its Live Matter group.

use manifold_core::effect_graph_def::EffectGraphDef;
use serde_json::{Value, json};

/// The matter components' node ids inside the Live Matter group, x, y and z.
pub(super) const MATTER_FACE_NODES: [&str; 3] = ["matter_face_u", "matter_face_v", "matter_face_w"];

/// The node between one axis's component and the frame in a `consumer` scene.
pub(super) const FACE_CONSUMER: &str = "matter_face_consumer";

/// The consumer's lattice: an even factoring of the 64-cell preset's 64 × 65
/// × 64 v faces, so its standalone run covers exactly that face array.
pub(super) const CONSUMER_LATTICE: [f32; 3] = [130.0, 64.0, 32.0];

/// `WaterDamBreakMatter.json` with `node.matter_face_component` × 3 on
/// matter_state's grid feeding matter_frame's face inputs. `consumer` puts a
/// GPU per-element atom sized from its input between that axis's component
/// and the frame, so the two fold into one fused dispatch. The consumer is
/// node.cosine_poisson_divide, chosen for its shape, not its meaning: the
/// only such atoms over Array<f32> are the cosine ones and
/// node.divide_by_value, whose gathered divisor shrinks a fused region to
/// one element (BUG-sk62, divide_by_value fused region shrinks to its divisor). Without
/// `collider` the moving box is unwired, so the tank holds only the column
/// and the pool, as SWASH's Dam Break does.
pub(super) fn matter_dam_break_faces(consumer: Option<usize>, collider: bool) -> EffectGraphDef {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/WaterDamBreakMatter.json");
    let mut preset: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("preset reads")).expect("preset parses");
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
            let consumer = next + 3;
            let [x, y, z] = CONSUMER_LATTICE;
            inner["nodes"].as_array_mut().expect("group nodes").push(json!({
                "id": consumer, "typeId": "node.cosine_poisson_divide", "nodeId": FACE_CONSUMER,
                "params": {
                    "nodes_x": {"type": "Float", "value": x},
                    "nodes_y": {"type": "Float", "value": y},
                    "nodes_z": {"type": "Float", "value": z},
                },
            }));
            wires.push(json!({"fromNode": id, "fromPort": "out", "toNode": consumer, "toPort": "values"}));
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
