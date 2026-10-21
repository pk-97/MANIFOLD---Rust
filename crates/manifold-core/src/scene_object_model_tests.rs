use serde_json::{Value, json};

use super::*;
use crate::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;

fn node(id: u32, node_id: &str, type_id: &str) -> Value {
    json!({"id": id, "nodeId": node_id, "typeId": type_id})
}

fn wire(from: u32, from_port: &str, to: u32, to_port: &str) -> Value {
    json!({"fromNode": from, "fromPort": from_port, "toNode": to, "toPort": to_port})
}

fn group(id: u32, node_id: &str, outputs: &[&str], nodes: Vec<Value>, wires: Vec<Value>) -> Value {
    json!({
        "id": id, "nodeId": node_id, "typeId": "group", "handle": node_id,
        "group": {
            "interface": {"inputs": [], "outputs": outputs.iter().map(|name| json!({"name": name, "portType": "SceneObject"})).collect::<Vec<_>>()},
            "nodes": nodes, "wires": wires
        }
    })
}

fn graph(nodes: Vec<Value>, wires: Vec<Value>) -> EffectGraphDef {
    serde_json::from_value(json!({"version": 2, "nodes": nodes, "wires": wires})).expect("graph parses")
}

fn scene() -> SceneNodeRef {
    SceneNodeRef { scope: Vec::new(), node: NodeId::new("scene") }
}

fn reference(scope: &[&str], node: &str) -> SceneNodeRef {
    SceneNodeRef { scope: scope.iter().map(|id| NodeId::new(*id)).collect(), node: NodeId::new(node) }
}

/// A water group: domain → step → surface → water object (`water`), and a
/// foam look fed from the step, both output; plus a placed Inflow group
/// wired into the domain's role_0.
fn water_group(id: u32, name: &str, water_port: &str) -> Value {
    let inflow = json!({
        "id": 9, "nodeId": format!("{name}_inflow_group"), "typeId": "group", "handle": "Inflow 1",
        "group": {
            "interface": {"inputs": [], "outputs": [{"name": "role", "portType": "FluidRole"}]},
            "nodes": [node(1, &format!("{name}_inflow_transform"), "node.transform_3d"),
                node(2, &format!("{name}_inflow"), "node.fluid_role_source"),
                node(3, &format!("{name}_inflow_out"), "system.group_output")],
            "wires": [wire(1, "transform", 2, "transform"), wire(2, "role", 3, "role")]
        }
    });
    group(id, name, &[water_port, "foam"], vec![
        node(1, &format!("{name}_domain"), GPU_FLIP_DOMAIN_TYPE_ID),
        node(2, &format!("{name}_step"), "node.whitewater_step"),
        node(3, &format!("{name}_surface"), "node.marching_cubes"),
        node(4, &format!("{name}_water"), "node.scene_object"),
        node(5, &format!("{name}_foam_mesh"), "node.platonic_solid_mesh"),
        node(6, &format!("{name}_foam"), "node.scene_object"),
        node(7, &format!("{name}_out"), "system.group_output"),
        inflow,
    ], vec![
        wire(1, "epoch", 2, "epoch"),
        wire(1, "particles", 3, "particles"),
        wire(3, "vertices", 4, "vertices"),
        wire(2, "foam", 6, "instances"),
        wire(5, "vertices", 6, "vertices"),
        wire(4, "object", 7, water_port),
        wire(6, "object", 7, "foam"),
        wire(9, "role", 1, "role_0"),
    ])
}

fn scene_nodes(extra: Vec<Value>) -> Vec<Value> {
    let mut nodes = vec![node(100, "scene", "node.render_scene"), node(101, "final", "system.final_output")];
    nodes.extend(extra);
    nodes
}

/// BUG-4lfm: a GPU-surface water whose object leaves the group on a port not
/// named the family way is still Water, recognised by its domain.
#[test]
fn scene_object_model_recognises_water_by_its_domain() {
    let graph = graph(
        scene_nodes(vec![water_group(1, "lake", "water")]),
        vec![wire(1, "water", 100, "object_0"), wire(1, "foam", 100, "object_3"), wire(100, "color", 101, "in")],
    );
    let models = scene_object_models(&graph, &scene()).expect("models");
    let objects: Vec<_> = models.iter().map(|model| (model.object.node.as_str(), model.kind, model.slot)).collect();
    assert_eq!(objects, [
        ("lake_water", SceneObjectRowKind::PhysicsParent, Some(0)),
        ("lake_foam", SceneObjectRowKind::LookChild, Some(3)),
        ("lake_inflow", SceneObjectRowKind::Object, None),
    ]);
    let water = &models[0];
    assert_eq!(water.owner, reference(&[], "lake"));
    assert_eq!(water.liquid_domain, Some(reference(&["lake"], "lake_domain")));
    assert_eq!(water.controls.first(), Some(&reference(&["lake"], "lake_domain")), "the simulation leads");
    assert!(water.controls.contains(&reference(&["lake"], "lake_step")), "whitewater on the domain's clock");
    assert!(!water.capabilities.duplicate && water.capabilities.delete);
    let foam = &models[1];
    assert_eq!(foam.parent, Some(water.object.clone()));
    assert_eq!(foam.controls, [reference(&["lake"], "lake_foam_mesh")]);
    assert!(foam.capabilities.hide && !foam.capabilities.delete);
    let inflow = &models[2];
    assert_eq!(inflow.parent, Some(water.object.clone()));
    assert_eq!(inflow.owner, reference(&["lake"], "lake_inflow_group"));
    assert!(inflow.capabilities.delete && inflow.capabilities.transform);
    assert!(!water.controls.iter().any(|control| inflow.controls.contains(control)), "the Inflow owns its own controls");
}

/// Grouping alone makes no family: two domains in one group stay two
/// independent objects.
#[test]
fn scene_object_model_keeps_independent_domains_apart() {
    let mut lake = water_group(1, "lake", "water");
    let sea = water_group(2, "sea", "water");
    lake["group"]["nodes"].as_array_mut().unwrap().push(sea);
    let graph = graph(
        scene_nodes(vec![lake]),
        vec![wire(1, "water", 100, "object_0"), wire(100, "color", 101, "in")],
    );
    let models = scene_object_models(&graph, &scene()).expect("models");
    assert!(models.iter().all(|model| model.kind == SceneObjectRowKind::Object), "{models:#?}");
}

/// Document ids repeat across scopes; rows keep their scoped identities.
#[test]
fn physics_boundary_scoped_duplicate_doc_ids() {
    let graph = graph(
        scene_nodes(vec![water_group(1, "lake", "water"), water_group(2, "sea", "water")]),
        vec![
            wire(1, "water", 100, "object_0"), wire(1, "foam", 100, "object_1"),
            wire(2, "water", 100, "object_2"), wire(2, "foam", 100, "object_3"),
            wire(100, "color", 101, "in"),
        ],
    );
    let models = scene_object_models(&graph, &scene()).expect("models");
    let parents: Vec<_> = models.iter().filter(|model| model.kind == SceneObjectRowKind::PhysicsParent).collect();
    assert_eq!(parents.len(), 2);
    assert_eq!(parents[0].doc_path, [1, 4]);
    assert_eq!(parents[1].doc_path, [2, 4]);
    assert_ne!(parents[0].object, parents[1].object);
    for model in models.iter().filter(|model| model.kind == SceneObjectRowKind::LookChild) {
        let parent = model.parent.as_ref().expect("a look has its water");
        assert_eq!(parent.scope, model.object.scope, "{:?} sits under its own water", model.object);
    }
}

/// An imported model's group is the parent of its parts.
#[test]
fn scene_object_model_lists_imported_model_parts_under_their_group() {
    let model = group(1, "statue", &["object", "object_1"], vec![
        node(1, "statue_root", "node.transform_3d"),
        node(2, "statue_head", "node.scene_object"),
        node(3, "statue_body", "node.scene_object"),
        node(4, "statue_out", "system.group_output"),
    ], vec![
        wire(1, "transform", 2, "parent_transform"),
        wire(1, "transform", 3, "parent_transform"),
        wire(2, "object", 4, "object"),
        wire(3, "object", 4, "object_1"),
    ]);
    let graph = graph(
        scene_nodes(vec![model]),
        vec![wire(1, "object", 100, "object_0"), wire(1, "object_1", 100, "object_1"), wire(100, "color", 101, "in")],
    );
    let models = scene_object_models(&graph, &scene()).expect("models");
    let rows: Vec<_> = models.iter().map(|model| (model.object.node.as_str(), model.kind, model.parent.clone())).collect();
    assert_eq!(rows, [
        ("statue", SceneObjectRowKind::Group, None),
        ("statue_head", SceneObjectRowKind::Object, Some(reference(&[], "statue"))),
        ("statue_body", SceneObjectRowKind::Object, Some(reference(&[], "statue"))),
    ]);
    assert_eq!(models[0].doc_path, [1]);
    assert_eq!(models[0].transform, Some(reference(&["statue"], "statue_root")));
}

/// An object group: one draw placed by its own transform and, through the
/// group input, by the enclosing group's transform.
fn object_group(id: u32, name: &str, port: &str) -> Value {
    json!({
        "id": id, "nodeId": name, "typeId": "group", "handle": name,
        "group": {
            "interface": {
                "inputs": [{"name": "parent", "portType": "Transform"}],
                "outputs": [{"name": port, "portType": "SceneObject"}]
            },
            "nodes": [
                node(1, &format!("{name}_in"), "system.group_input"),
                node(2, &format!("{name}_transform"), "node.transform_3d"),
                node(3, &format!("{name}_draw"), "node.scene_object"),
                node(4, &format!("{name}_out"), "system.group_output"),
            ],
            "wires": [
                wire(2, "transform", 3, "transform"),
                wire(1, "parent", 3, "parent_transform"),
                wire(3, "object", 4, port),
            ]
        }
    })
}

/// A scene group: `children` (object and group graph groups) placed by its
/// own group transform, which takes the enclosing group's on `parent`.
fn scene_group(id: u32, name: &str, children: Vec<(u32, Value, &str)>, nested: bool) -> Value {
    let mut nodes = vec![
        node(1, &format!("{name}_in"), "system.group_input"),
        node(2, &format!("{name}_group_transform"), "node.transform_3d"),
        node(3, &format!("{name}_out"), "system.group_output"),
    ];
    let mut wires = Vec::new();
    if nested {
        wires.push(wire(1, "parent", 2, "parent"));
    }
    let mut outputs = Vec::new();
    for (child_id, child, port) in children {
        nodes.push(child);
        wires.push(wire(2, "transform", child_id, "parent"));
        wires.push(wire(child_id, port, 3, port));
        outputs.push(json!({"name": port, "portType": "SceneObject"}));
    }
    json!({
        "id": id, "nodeId": name, "typeId": "group", "handle": name,
        "group": {
            "interface": {"inputs": [{"name": "parent", "portType": "Transform"}], "outputs": outputs},
            "nodes": nodes, "wires": wires
        }
    })
}

/// Groups nest: every row's parent is its nearest group at any depth, and
/// each group lists its contents in render slot order straight after it.
#[test]
fn scene_object_model_nests_groups_three_deep() {
    // outer { rock_a, middle { rock_b, inner { rock_c, rock_d } } }, rock_e loose.
    let inner = scene_group(30, "inner", vec![(31, object_group(31, "rock_c", "c"), "c"), (32, object_group(32, "rock_d", "d"), "d")], true);
    let middle = scene_group(20, "middle", vec![(21, object_group(21, "rock_b", "b"), "b"), (30, inner, "c")], true);
    let mut middle = middle;
    // The inner group re-exports both of its objects.
    middle["group"]["wires"].as_array_mut().unwrap().push(wire(30, "d", 3, "d"));
    middle["group"]["interface"]["outputs"].as_array_mut().unwrap().push(json!({"name": "d", "portType": "SceneObject"}));
    let mut outer = scene_group(10, "outer", vec![(11, object_group(11, "rock_a", "a"), "a"), (20, middle, "b")], false);
    for port in ["c", "d"] {
        outer["group"]["wires"].as_array_mut().unwrap().push(wire(20, port, 3, port));
        outer["group"]["interface"]["outputs"].as_array_mut().unwrap().push(json!({"name": port, "portType": "SceneObject"}));
    }
    let loose = object_group(40, "rock_e", "object");
    let graph = graph(
        scene_nodes(vec![outer, loose]),
        vec![
            wire(40, "object", 100, "object_0"),
            wire(10, "a", 100, "object_1"),
            wire(10, "d", 100, "object_2"),
            wire(10, "b", 100, "object_3"),
            wire(10, "c", 100, "object_4"),
            wire(100, "color", 101, "in"),
        ],
    );
    let models = scene_object_models(&graph, &scene()).expect("models");
    let rows: Vec<_> = models
        .iter()
        .map(|model| (model.object.node.as_str(), model.kind, model.parent.as_ref().map(|parent| parent.node.as_str())))
        .collect();
    use SceneObjectRowKind::{Group, Object};
    assert_eq!(rows, [
        ("rock_e_draw", Object, None),
        ("outer", Group, None),
        ("rock_a_draw", Object, Some("outer")),
        ("middle", Group, Some("outer")),
        ("inner", Group, Some("middle")),
        ("rock_d_draw", Object, Some("inner")),
        ("rock_c_draw", Object, Some("inner")),
        ("rock_b_draw", Object, Some("middle")),
    ]);
    let inner = models.iter().find(|model| model.object.node.as_str() == "inner").unwrap();
    assert_eq!(inner.object.scope, [NodeId::new("outer"), NodeId::new("middle")]);
    assert_eq!(inner.transform, Some(reference(&["outer", "middle", "inner"], "inner_group_transform")));
    assert_eq!(inner.doc_path, [10, 20, 30]);
    // Only a top-level group takes the whole-group commands for now.
    let outer = models.iter().find(|model| model.object.node.as_str() == "outer").unwrap();
    assert!(outer.capabilities.enable_physics && outer.capabilities.delete);
    assert!(inner.capabilities.transform && inner.capabilities.rename && !inner.capabilities.enable_physics && !inner.capabilities.delete);
}

/// A group with physics draws through its body's pose; the group's own
/// transform is the authored one behind the body.
#[test]
fn scene_object_model_reads_a_physics_groups_transform_behind_its_body() {
    let model = group(1, "statue", &["object"], vec![
        node(1, "statue_root", "node.transform_3d"),
        node(2, "statue_head", "node.scene_object"),
        node(3, "statue_body", "node.rigid_body"),
        node(4, "statue_out", "system.group_output"),
    ], vec![
        wire(1, "transform", 3, "transform"),
        wire(3, "pose", 2, "parent_transform"),
        wire(2, "object", 4, "object"),
    ]);
    let graph = graph(scene_nodes(vec![model]), vec![wire(1, "object", 100, "object_0"), wire(100, "color", 101, "in")]);
    let models = scene_object_models(&graph, &scene()).expect("models");
    assert_eq!(models[0].kind, SceneObjectRowKind::Group);
    assert_eq!(models[0].transform, Some(reference(&["statue"], "statue_root")));
    assert_eq!(models[1].parent, Some(reference(&[], "statue")));
}

#[test]
fn physics_boundary_remove_placed_source() {
    let mut graph = graph(
        scene_nodes(vec![water_group(1, "lake", "water")]),
        vec![wire(1, "water", 100, "object_0"), wire(100, "color", 101, "in")],
    );
    graph.preset_metadata = Some(serde_json::from_value(json!({
        "id": "t", "displayName": "T", "category": "Diagnostic", "oscPrefix": "t",
        "params": [{"id": "inflow_y", "name": "Height", "defaultValue": 0.0, "min": 0.0, "max": 1.0},
            {"id": "depth", "name": "Depth", "defaultValue": 0.0, "min": 0.0, "max": 1.0}],
        "bindings": [
            {"id": "inflow_y", "defaultValue": 0.0, "target": {"kind": "node", "nodeId": "lake_inflow_transform", "param": "pos_y"}},
            {"id": "depth", "defaultValue": 0.0, "target": {"kind": "node", "nodeId": "lake_domain", "param": "fill_height"}}
        ]
    })).unwrap());
    let edit = remove_placed_source(&graph, &reference(&["lake", "lake_inflow_group"], "lake_inflow")).expect("removes");
    let body = edit.graph.nodes.iter().find(|node| node.id == 1).unwrap().group.as_deref().unwrap();
    assert!(!body.nodes.iter().any(|node| node.id == 9));
    assert!(!body.wires.iter().any(|wire| wire.from_node == 9 || wire.to_port == "role_0"));
    assert_eq!(edit.removed_param_ids, ["inflow_y"]);
    let metadata = edit.graph.preset_metadata.as_ref().unwrap();
    assert_eq!(metadata.params.iter().map(|spec| spec.id.as_str()).collect::<Vec<_>>(), ["depth"]);
    let models = scene_object_models(&edit.graph, &scene()).unwrap();
    assert!(!models.iter().any(|model| model.object.node.as_str() == "lake_inflow"));

    assert!(remove_placed_source(&graph, &reference(&["lake"], "lake_water")).is_err(), "only a placed source");

    // An Inflow group wrapped in another group: the tree lists the wrapper,
    // and deleting removes the wrapper, never refuses.
    let mut nested = graph.clone();
    let lake = nested.nodes.iter_mut().find(|node| node.id == 1).unwrap().group.as_deref_mut().unwrap();
    let inflow = lake.nodes.iter().position(|node| node.id == 9).unwrap();
    let inner = lake.nodes.remove(inflow);
    let mut wrapper: EffectGraphNode = serde_json::from_value(group(9, "lake_wrapper", &["role"],
        vec![node(5, "lake_wrapper_out", "system.group_output")], vec![wire(4, "role", 5, "role")])).unwrap();
    let mut inner = inner;
    inner.id = 4;
    wrapper.group.as_deref_mut().unwrap().nodes.push(inner);
    lake.nodes.push(wrapper);
    let models = scene_object_models(&nested, &scene()).unwrap();
    let listed = models.iter().find(|model| model.object.node.as_str() == "lake_inflow").expect("the nested source is listed");
    assert_eq!(listed.owner, reference(&["lake"], "lake_wrapper"));
    let edit = remove_placed_source(&nested, &reference(&["lake", "lake_wrapper", "lake_inflow_group"], "lake_inflow")).expect("removes the wrapper");
    let body = edit.graph.nodes.iter().find(|node| node.id == 1).unwrap().group.as_deref().unwrap();
    assert!(!body.nodes.iter().any(|node| node.id == 9) && !body.wires.iter().any(|wire| wire.to_port == "role_0"));
    assert_eq!(edit.removed_param_ids, ["inflow_y"]);
}

/// A loose object drawing a physics body's `instances` is a copy group: its
/// instancer is the body, its card carries the body and the transform placing
/// the group, and it cannot make copies again.
#[test]
fn scene_object_model_reads_a_copy_group_from_the_instances_it_draws() {
    let graph = graph(
        scene_nodes(vec![
            node(2, "boxes", "node.scene_object"),
            node(3, "boxes_body", "node.rigid_body"),
            node(4, "boxes_pos", "node.transform_3d"),
            node(5, "world", "node.physics_world"),
        ]),
        vec![
            wire(4, "transform", 3, "transform"),
            wire(3, "instances", 2, "instances"),
            wire(3, "active_count", 2, "instance_count"),
            wire(3, "body", 5, "copies_0"),
            wire(2, "object", 100, "object_0"),
            wire(100, "color", 101, "in"),
        ],
    );
    let models = scene_object_models(&graph, &scene()).expect("models");
    let boxes = models.iter().find(|model| model.object.node.as_str() == "boxes").expect("the drawing object");
    assert_eq!(boxes.copy_group(), Some(&reference(&[], "boxes_body")));
    assert_eq!(boxes.swarm(), None);
    assert!(!boxes.capabilities.make_copies, "a copy group does not make copies again");
    assert_eq!(boxes.controls, [reference(&[], "boxes_body"), reference(&[], "boxes_pos")]);
}
