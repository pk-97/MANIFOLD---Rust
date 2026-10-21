//! Scene groups nest: the Scene tab rows and the gizmo frames of a three-deep
//! group chain, read from a graph through the one scene-object model.

use serde_json::{Value, json};

use super::*;
use manifold_node_engine::scene::transform::Transform;

fn node(id: u32, node_id: &str, type_id: &str) -> Value {
    json!({"id": id, "nodeId": node_id, "typeId": type_id})
}

fn transform(id: u32, node_id: &str, pos_x: f32, rot_z: f32) -> Value {
    json!({"id": id, "nodeId": node_id, "typeId": "node.transform_3d", "params": {
        "pos_x": {"type": "Float", "value": pos_x},
        "rot_z": {"type": "Float", "value": rot_z},
    }})
}

fn wire(from: u32, from_port: &str, to: u32, to_port: &str) -> Value {
    json!({"fromNode": from, "fromPort": from_port, "toNode": to, "toPort": to_port})
}

fn group(id: u32, name: &str, outputs: &[&str], nodes: Vec<Value>, wires: Vec<Value>) -> Value {
    json!({
        "id": id, "nodeId": name, "typeId": "group", "handle": name,
        "group": {
            "interface": {
                "inputs": [{"name": "parent", "portType": "Transform"}],
                "outputs": outputs.iter().map(|name| json!({"name": name, "portType": "SceneObject"})).collect::<Vec<_>>()
            },
            "nodes": nodes, "wires": wires
        }
    })
}

/// An object: one draw placed by its own transform inside the enclosing
/// group's frame, which arrives on the object group's `parent` input.
fn object(id: u32, name: &str, port: &str, pos_x: f32) -> Value {
    group(id, name, &[port], vec![
        node(1, &format!("{name}_in"), "system.group_input"),
        transform(2, &format!("{name}_transform"), pos_x, 0.0),
        node(3, &format!("{name}_draw"), "node.scene_object"),
        node(4, &format!("{name}_out"), "system.group_output"),
    ], vec![
        wire(2, "transform", 3, "transform"),
        wire(1, "parent", 3, "parent_transform"),
        wire(3, "object", 4, port),
    ])
}

/// outer (x 2, turned a quarter) { rock_a, inner (x 1) { rock_b (x 0.5) } }.
fn nested_def() -> EffectGraphDef {
    let quarter = std::f32::consts::FRAC_PI_2;
    let inner = group(20, "inner", &["b"], vec![
        node(1, "inner_in", "system.group_input"),
        transform(2, "inner_group_transform", 1.0, 0.0),
        node(3, "inner_out", "system.group_output"),
        object(21, "rock_b", "b", 0.5),
    ], vec![
        wire(1, "parent", 2, "parent"),
        wire(2, "transform", 21, "parent"),
        wire(21, "b", 3, "b"),
    ]);
    let outer = group(10, "outer", &["a", "b"], vec![
        transform(2, "outer_group_transform", 2.0, quarter),
        node(3, "outer_out", "system.group_output"),
        object(11, "rock_a", "a", 0.0),
        inner,
    ], vec![
        wire(2, "transform", 11, "parent"),
        wire(2, "transform", 20, "parent"),
        wire(11, "a", 3, "a"),
        wire(20, "b", 3, "b"),
    ]);
    serde_json::from_value(json!({"version": 2, "nodes": [
        {"id": 100, "nodeId": "scene", "typeId": "node.render_scene", "params": {"objects": {"type": "Float", "value": 2.0}}},
        node(101, "final", "system.final_output"),
        outer,
    ], "wires": [
        wire(10, "a", 100, "object_0"),
        wire(10, "b", 100, "object_1"),
        wire(100, "color", 101, "in"),
    ]}))
    .expect("graph parses")
}

fn known(vm: &SceneVm) -> Vec<&SceneObjectKnownRow> {
    vm.objects
        .iter()
        .filter_map(|object| match object {
            SceneObjectVm::Known(row) => Some(row.as_ref()),
            SceneObjectVm::Custom { .. } => None,
        })
        .collect()
}

#[test]
fn nested_group_rows_carry_their_kind_parent_and_own_transform() {
    let vm = SceneVm::from_def(&nested_def()).expect("a scene");
    let rows = known(&vm);
    let summary: Vec<_> = rows
        .iter()
        .map(|row| (row.address.node.as_str(), row.kind, row.is_group, row.parent.as_ref().map(|parent| parent.node.as_str())))
        .collect();
    assert_eq!(summary, [
        ("outer", SceneObjectRowKind::Group, true, None),
        ("rock_a_draw", SceneObjectRowKind::Object, false, Some("outer")),
        ("inner", SceneObjectRowKind::Group, true, Some("outer")),
        ("rock_b_draw", SceneObjectRowKind::Object, false, Some("inner")),
    ]);
    let inner = rows[2];
    let transform = inner.transform.as_ref().expect("the inner group reads its own transform");
    assert_eq!(transform.node.as_str(), "inner_group_transform");
    assert_eq!(transform.pos_addr.0.scope_path, [10, 20], "its rows write inside the inner group");
}

#[test]
fn each_rows_gizmo_frame_is_its_whole_group_chain() {
    let vm = SceneVm::from_def(&nested_def()).expect("a scene");
    let quarter = std::f32::consts::FRAC_PI_2;
    let outer = Transform { pos: [2.0, 0.0, 0.0], rot_euler: [0.0, 0.0, quarter], ..Transform::default() };
    let inner = Transform { pos: [1.0, 0.0, 0.0], parent: outer.world(), ..Transform::default() };
    let frame_of = |name: &str| {
        vm.shapes
            .iter()
            .find(|shape| matches!(&shape.owner, ShapeOwner::Object(owner) if owner.node.as_str() == name))
            .unwrap_or_else(|| panic!("{name} has a gizmo shape"))
            .frame
    };
    assert_eq!(frame_of("outer"), None, "a top-level group sits in the world");
    assert_eq!(frame_of("inner"), Some(outer.world()));
    let rock_b = frame_of("rock_b_draw").expect("an object two groups deep has a frame");
    let want = inner.world();
    for c in 0..4 {
        for r in 0..4 {
            assert!((rock_b[c][r] - want[c][r]).abs() < 1e-6, "rock_b's frame is outer times inner");
        }
    }
    // rock_b at x 0.5 in inner's frame: inner sits 1 along outer's turned X
    // (world +Y), so the object is at world (2, 1.5, 0).
    let origin = [0, 1, 2].map(|i| rock_b[3][i] + rock_b[0][i] * 0.5);
    assert!((origin[0] - 2.0).abs() < 1e-5 && (origin[1] - 1.5).abs() < 1e-5 && origin[2].abs() < 1e-5, "{origin:?}");
}
