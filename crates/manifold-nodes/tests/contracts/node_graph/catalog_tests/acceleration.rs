//! CPU graph proofs for physical recipients of force recipes.

use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier;
use manifold_node_engine::load::expand::{SceneModifierExpandError, prepare_scene_modifiers};
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};
use manifold_core::scene_modifier_edit::insert_scene_modifier;
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
use std::collections::{BTreeMap, BTreeSet};

const PHYSICS_SOLIDS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/generator-presets/PhysicsSolids.json"
));
const UNIFORM_FORCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/scene-modifier-presets/UniformForce.json"
));

fn host() -> EffectGraphDef {
    serde_json::from_str(PHYSICS_SOLIDS).expect("PhysicsSolids fixture parses")
}
fn recipe() -> EffectGraphDef {
    serde_json::from_str(UNIFORM_FORCE).expect("UniformForce fixture parses")
}

/// Follow scene_modifier_edit::build_action Add and Retarget: prepare and
/// insert the all-object force, then resolve frames and apply the selection.
#[test]
fn scene_modifier_grouped_water_add_and_retarget_expand() {
    use manifold_nodes_scene::node_graph::scene_modifier_authoring::scene_modifier_objects;
    use manifold_node_engine::load::expand::resolve_modifier_mesh_frames;
    use manifold_core::scene_modifier_edit::retarget_scene_modifier;
    use manifold_core::scene_index::FlatSceneIndex;

    let owner = manifold_nodes::bundled_presets::bundled_preset_def(
        &manifold_core::PresetTypeId::new("WaterDamBreakGpuFlip"),
    ).expect("shipped Dam Break after load migrations").clone();
    let scene = scene(&owner);
    let water = SceneNodeRef::locate(&owner, &NodeId::new("water_object")).unwrap();
    assert_eq!(water.scope.len(), 1);
    assert!(scene_modifier_objects(&owner, &scene).unwrap().contains(&water));
    let owner = attach(&owner, "force", SceneTargetSelection::AllObjects);
    prepare_scene_modifiers(&owner, &PrimitiveRegistry::with_builtin())
        .expect("performer Add Uniform Force expands");
    manifold_node_engine::runtime::PresetRuntime::from_def(owner.clone(), &PrimitiveRegistry::with_builtin(), None)
        .expect("performer Add Uniform Force builds a runtime");
    let mut instance = owner.scene_modifiers[0].clone();
    instance.targets = SceneTargetSelection::Explicit { objects: vec![water.clone()] };
    let frames = resolve_modifier_mesh_frames(&owner, &instance).unwrap();
    let owner = retarget_scene_modifier(&owner, &instance.id, instance.targets, frames).unwrap().graph;
    assert!(FlatSceneIndex::build(&owner).unwrap().scene_objects(&scene).unwrap().contains(&water));
    prepare_scene_modifiers(&owner, &PrimitiveRegistry::with_builtin())
        .expect("performer retarget to grouped Water expands");
    manifold_node_engine::runtime::PresetRuntime::from_def(owner, &PrimitiveRegistry::with_builtin(), None)
        .expect("performer retarget to grouped Water builds a runtime");
}
fn reference(def: &EffectGraphDef, ty: &str, id: &str) -> SceneNodeRef {
    let node = def
        .nodes
        .iter()
        .find(|node| node.type_id == ty && node.node_id.as_str() == id)
        .unwrap_or_else(|| panic!("{ty} {id} exists"));
    SceneNodeRef {
        scope: Vec::new(),
        node: node.node_id.clone(),
    }
}
fn node_id(def: &EffectGraphDef, id: &str) -> u32 {
    def.nodes
        .iter()
        .find(|node| node.node_id.as_str() == id)
        .unwrap_or_else(|| panic!("node {id} exists"))
        .id
}
fn scene(def: &EffectGraphDef) -> SceneNodeRef {
    reference(def, "node.render_scene", "scene")
}
fn wire(def: &mut EffectGraphDef, from: &str, fp: &str, to: &str, tp: &str) {
    let from_node = node_id(def, from);
    let to_node = node_id(def, to);
    def.wires.push(EffectGraphWire {
        from_node,
        from_port: fp.into(),
        to_node,
        to_port: tp.into(),
    });
}
fn remove_wire(def: &mut EffectGraphDef, from: &str, fp: &str, to: &str, tp: &str) {
    let from_id = node_id(def, from);
    let to_id = node_id(def, to);
    def.wires.retain(|wire| {
        !(wire.from_node == from_id
            && wire.from_port == fp
            && wire.to_node == to_id
            && wire.to_port == tp)
    });
}
fn attach(owner: &EffectGraphDef, id: &str, targets: SceneTargetSelection) -> EffectGraphDef {
    let instance =
        prepare_new_scene_modifier(owner, &recipe(), NodeId::new(id), scene(owner), targets)
            .unwrap();
    insert_scene_modifier(owner, owner.scene_modifiers.len(), instance)
        .unwrap()
        .graph
}
fn prepared(owner: &EffectGraphDef) -> EffectGraphDef {
    prepare_scene_modifiers(owner, &PrimitiveRegistry::with_builtin())
        .unwrap()
        .def
}
fn target_wires<'a>(def: &'a EffectGraphDef, port: &str) -> Vec<&'a EffectGraphWire> {
    let world = node_id(def, "physics_demo_40");
    def.wires
        .iter()
        .filter(|wire| wire.to_node == world && wire.to_port == port)
        .collect()
}
fn clone_node(def: &EffectGraphDef, source: &str, id: u32, stable: &str) -> EffectGraphNode {
    let mut node = def
        .nodes
        .iter()
        .find(|node| node.node_id.as_str() == source)
        .unwrap()
        .clone();
    node.id = id;
    node.node_id = NodeId::new(stable);
    node.handle = Some(stable.into());
    node
}
fn add_empty_fluid(def: &mut EffectGraphDef) {
    def.nodes.push(EffectGraphNode {
        id: 900,
        handle: Some("test_fluid".into()),
        node_id: NodeId::new("test_fluid"),
        type_id: manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID.into(),
        params: BTreeMap::new(),
        exposed_params: BTreeSet::new(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    });
    let mut mesh = clone_node(def, "test_fluid", 901, "test_fluid_mesh");
    mesh.type_id = "node.grid_mesh".into();
    def.nodes.push(mesh);
    wire(def, "test_fluid", "cell_size", "test_fluid_mesh", "size_x");
    remove_wire(
        def,
        "physics_demo_102",
        "vertices",
        "physics_demo_104",
        "vertices",
    );
    remove_wire(
        def,
        "physics_demo_40",
        "pose_0",
        "physics_demo_104",
        "transform",
    );
    wire(
        def,
        "test_fluid_mesh",
        "vertices",
        "physics_demo_104",
        "vertices",
    );
}

#[test]
fn force_body_targets_only_selected_world_slot() {
    let owner = host();
    let expanded = prepared(&attach(
        &owner,
        "body_force",
        SceneTargetSelection::Explicit {
            objects: vec![reference(&owner, "node.scene_object", "physics_demo_104")],
        },
    ));
    assert_eq!(target_wires(&expanded, "body_acceleration_0").len(), 1);
    for slot in 1..6 {
        assert!(target_wires(&expanded, &format!("body_acceleration_{slot}")).is_empty());
    }
}

#[test]
fn force_copies_target_uses_world_instances_input() {
    let mut owner = host();
    remove_wire(
        &mut owner,
        "physics_demo_40",
        "pose_0",
        "physics_demo_104",
        "transform",
    );
    wire(
        &mut owner,
        "physics_demo_40",
        "instances",
        "physics_demo_104",
        "instances",
    );
    wire(&mut owner, "physics_demo_101", "body", "physics_demo_40", "copies");
    let expanded = prepared(&attach(
        &owner,
        "copies_force",
        SceneTargetSelection::Explicit {
            objects: vec![reference(&owner, "node.scene_object", "physics_demo_104")],
        },
    ));
    assert_eq!(target_wires(&expanded, "copies_acceleration").len(), 1);
    assert!(target_wires(&expanded, "body_acceleration_0").is_empty());
}

#[test]
fn force_fluid_target_uses_surface_acceleration_input() {
    let mut owner = host();
    add_empty_fluid(&mut owner);
    let expanded = prepared(&attach(
        &owner,
        "fluid_force",
        SceneTargetSelection::Explicit {
            objects: vec![reference(&owner, "node.scene_object", "physics_demo_104")],
        },
    ));
    let fluid = node_id(&expanded, "test_fluid");
    assert_eq!(
        expanded
            .wires
            .iter()
            .filter(|wire| wire.to_node == fluid && wire.to_port == "acceleration_field")
            .count(),
        1
    );
    assert!(target_wires(&expanded, "body_acceleration_0").is_empty());
}

#[test]
fn two_forces_compose_through_previous_add_output() {
    let owner = host();
    let first = attach(
        &owner,
        "force_a",
        SceneTargetSelection::Explicit {
            objects: vec![reference(&owner, "node.scene_object", "physics_demo_104")],
        },
    );
    let second = attach(
        &first,
        "force_b",
        SceneTargetSelection::Explicit {
            objects: vec![reference(&owner, "node.scene_object", "physics_demo_104")],
        },
    );
    let expanded = prepared(&second);
    let adds: BTreeSet<_> = expanded
        .nodes
        .iter()
        .filter(|node| node.type_id == "node.add_vector_fields")
        .map(|node| node.id)
        .collect();
    assert_eq!(adds.len(), 2);
    let previous: Vec<_> = expanded
        .wires
        .iter()
        .filter(|wire| adds.contains(&wire.to_node) && wire.to_port == "a")
        .collect();
    assert_eq!(previous.len(), 2);
    assert_eq!(
        previous
            .iter()
            .filter(|wire| adds.contains(&wire.from_node))
            .count(),
        1
    );
    let seed = previous
        .iter()
        .find(|wire| !adds.contains(&wire.from_node))
        .unwrap();
    let zero = expanded
        .nodes
        .iter()
        .find(|node| node.id == seed.from_node)
        .unwrap();
    assert_eq!(zero.type_id, "node.uniform_vector_field");
    for component in ["x", "y", "z"] {
        assert_eq!(
            zero.params.get(component),
            Some(&manifold_core::effect_graph_def::SerializedParamValue::Float { value: 0.0 })
        );
    }
}

#[test]
fn grouped_parts_sharing_world_slot_emit_one_force_clone() {
    let mut owner = host();
    remove_wire(
        &mut owner,
        "physics_demo_40",
        "pose_1",
        "physics_demo_114",
        "transform",
    );
    wire(
        &mut owner,
        "physics_demo_40",
        "pose_0",
        "physics_demo_114",
        "transform",
    );
    let (nodes, wires) = manifold_core::group_edit::group_selection(
        owner.nodes,
        owner.wires,
        &BTreeSet::from([104, 114]),
        "Photoscan",
        (0.0, 0.0),
    )
    .unwrap();
    owner.nodes = nodes;
    owner.wires = wires;
    let group = owner
        .nodes
        .iter()
        .find(|node| node.group.is_some())
        .unwrap()
        .node_id
        .clone();
    let targets = SceneTargetSelection::Explicit {
        objects: ["physics_demo_104", "physics_demo_114"]
            .map(|node| SceneNodeRef {
                scope: vec![group.clone()],
                node: NodeId::new(node),
            })
            .to_vec(),
    };
    let mut attached = attach(&owner, "grouped_force", targets);
    let expanded = prepared(&attached);
    assert_eq!(target_wires(&expanded, "body_acceleration_0").len(), 1);
    assert!(target_wires(&expanded, "body_acceleration_1").is_empty());
    assert_eq!(
        expanded
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.add_vector_fields")
            .count(),
        1
    );
    attached
        .nodes
        .iter_mut()
        .find(|node| node.node_id == group)
        .unwrap()
        .handle = Some("Renamed scan".into());
    let saved: EffectGraphDef =
        serde_json::from_slice(&serde_json::to_vec(&attached).unwrap()).unwrap();
    assert_eq!(
        target_wires(&prepared(&saved), "body_acceleration_0").len(),
        1
    );
}

#[test]
fn all_objects_force_picks_up_body_added_after_attachment() {
    let owner = host();
    let attached = attach(&owner, "all_force", SceneTargetSelection::AllObjects);
    let mut later = attached.clone();
    for (source, id, stable) in [
        ("physics_demo_101", 500, "future_body"),
        ("physics_demo_100", 501, "future_transform"),
        ("physics_demo_102", 502, "future_mesh"),
        ("physics_demo_103", 503, "future_material"),
        ("physics_demo_104", 504, "future_object"),
    ] {
        later.nodes.push(clone_node(&later, source, id, stable));
    }
    wire(
        &mut later,
        "future_transform",
        "transform",
        "future_body",
        "transform",
    );
    wire(
        &mut later,
        "future_body",
        "body",
        "physics_demo_40",
        "body_6",
    );
    wire(&mut later, "future_body", "shape", "future_mesh", "shape");
    wire(
        &mut later,
        "future_mesh",
        "vertices",
        "future_object",
        "vertices",
    );
    wire(
        &mut later,
        "future_material",
        "out",
        "future_object",
        "material",
    );
    wire(
        &mut later,
        "physics_demo_40",
        "pose_6",
        "future_object",
        "transform",
    );
    wire(&mut later, "future_object", "object", "scene", "object_6");
    later
        .nodes
        .iter_mut()
        .find(|node| node.node_id.as_str() == "scene")
        .unwrap()
        .params
        .insert(
            "objects".into(),
            manifold_core::effect_graph_def::SerializedParamValue::Float { value: 7.0 },
        );
    assert_eq!(
        target_wires(&prepared(&later), "body_acceleration_6").len(),
        1
    );
}

#[test]
fn empty_and_nonphysical_force_targets_keep_shared_controls() {
    let owner = host();
    let empty = prepared(&attach(
        &owner,
        "empty_force",
        SceneTargetSelection::Explicit { objects: vec![] },
    ));
    assert!(
        empty
            .nodes
            .iter()
            .any(|node| node.type_id == "node.uniform_vector_field")
    );
    assert!(
        empty
            .wires
            .iter()
            .all(|wire| !wire.to_port.starts_with("body_acceleration_"))
    );
    let mut nonphysical = owner.clone();
    nonphysical.nodes.push(clone_node(
        &nonphysical,
        "physics_demo_104",
        600,
        "nonphysical_object",
    ));
    wire(
        &mut nonphysical,
        "physics_demo_102",
        "vertices",
        "nonphysical_object",
        "vertices",
    );
    wire(
        &mut nonphysical,
        "physics_demo_103",
        "out",
        "nonphysical_object",
        "material",
    );
    wire(
        &mut nonphysical,
        "nonphysical_object",
        "object",
        "scene",
        "object_6",
    );
    nonphysical.nodes.iter_mut().find(|node| node.node_id.as_str() == "scene").unwrap()
        .params.insert("objects".into(), manifold_core::effect_graph_def::SerializedParamValue::Float { value: 7.0 });
    let expanded = prepared(&attach(
        &nonphysical,
        "nonphysical_force",
        SceneTargetSelection::Explicit {
            objects: vec![reference(
                &nonphysical,
                "node.scene_object",
                "nonphysical_object",
            )],
        },
    ));
    assert!(
        expanded
            .nodes
            .iter()
            .any(|node| node.type_id == "node.uniform_vector_field")
    );
    assert!(
        expanded
            .wires
            .iter()
            .all(|wire| !wire.to_port.starts_with("body_acceleration_"))
    );
}

#[test]
fn inactive_force_retains_per_recipient_control_for_later_targets() {
    let mut recipe = recipe();
    let apply = recipe
        .nodes
        .iter_mut()
        .find(|node| node.node_id == "force_apply_stage")
        .unwrap()
        .group
        .as_mut()
        .unwrap();
    apply.nodes.push(
        serde_json::from_value(serde_json::json!({
            "id": 5, "nodeId": "recipient_scale", "typeId": "node.scale_vector_field"
        }))
        .unwrap(),
    );
    let output = apply
        .wires
        .iter_mut()
        .find(|wire| wire.to_node == 4)
        .unwrap();
    output.to_node = 5;
    output.to_port = "field".into();
    apply.wires.push(EffectGraphWire {
        from_node: 5,
        from_port: "out".into(),
        to_node: 4,
        to_port: "acceleration".into(),
    });
    let metadata = recipe.preset_metadata.as_mut().unwrap();
    let mut parameter = metadata
        .params
        .iter()
        .find(|param| param.id == "strength")
        .unwrap()
        .clone();
    parameter.id = "recipient_gain".into();
    parameter.default_value = 0.5;
    metadata.params.push(parameter);
    let mut binding = metadata
        .bindings
        .iter()
        .find(|binding| binding.id == "strength")
        .unwrap()
        .clone();
    binding.id = "recipient_gain".into();
    binding.default_value = 0.5;
    binding.target = manifold_core::effect_graph_def::BindingTarget::Node {
        node_id: NodeId::new("recipient_scale"),
        param: "strength".into(),
    };
    metadata.bindings.push(binding);
    let host = host();
    let instance = prepare_new_scene_modifier(
        &host,
        &recipe,
        NodeId::new("inactive"),
        scene(&host),
        SceneTargetSelection::Explicit { objects: vec![] },
    )
    .unwrap();
    let mut attached = insert_scene_modifier(&host, 0, instance).unwrap().graph;
    let inactive = prepare_scene_modifiers(&attached, &PrimitiveRegistry::with_builtin()).unwrap();
    assert!(
        inactive
            .routes
            .iter()
            .find(|route| route.local.node == "recipient_scale")
            .unwrap()
            .copies
            .is_empty()
    );
    assert!(attached.preset_metadata.as_ref().unwrap().bindings.iter().any(|binding| matches!(&binding.target,
        manifold_core::effect_graph_def::BindingTarget::SceneModifier { param_id, .. } if param_id == "recipient_gain")));
    attached.scene_modifiers[0].targets = SceneTargetSelection::Explicit {
        objects: vec![reference(&host, "node.scene_object", "physics_demo_104")],
    };
    let active = prepared(&attached);
    let scale = active
        .nodes
        .iter()
        .find(|node| {
            node.type_id == "node.scale_vector_field"
                && node.params.get("strength")
                    == Some(
                        &manifold_core::effect_graph_def::SerializedParamValue::Float {
                            value: 0.5,
                        },
                    )
        })
        .unwrap();
    assert!(active.preset_metadata.as_ref().unwrap().bindings.iter().any(|binding| matches!(&binding.target,
        manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } if node_id == &scale.node_id && param == "strength")));
}

#[test]
fn stale_target_and_ambiguous_physical_source_are_errors() {
    let owner = host();
    let mut stale_owner = attach(&owner, "stale_force", SceneTargetSelection::AllObjects);
    stale_owner.scene_modifiers[0].targets = SceneTargetSelection::Explicit {
        objects: vec![SceneNodeRef {
            scope: Vec::new(),
            node: NodeId::new("missing_object"),
        }],
    };
    assert!(matches!(
        prepare_scene_modifiers(&stale_owner, &PrimitiveRegistry::with_builtin()),
        Err(SceneModifierExpandError::MissingTarget { .. })
    ));
    let mut ambiguous = host();
    wire(
        &mut ambiguous,
        "physics_demo_40",
        "pose_1",
        "physics_demo_104",
        "parent_transform",
    );
    let force = prepare_new_scene_modifier(
        &ambiguous,
        &recipe(),
        NodeId::new("ambiguous_force"),
        scene(&ambiguous),
        SceneTargetSelection::Explicit {
            objects: vec![reference(
                &ambiguous,
                "node.scene_object",
                "physics_demo_104",
            )],
        },
    )
    .unwrap();
    let ambiguous_owner = insert_scene_modifier(&ambiguous, 0, force).unwrap().graph;
    assert!(matches!(
        prepare_scene_modifiers(&ambiguous_owner, &PrimitiveRegistry::with_builtin()),
        Err(SceneModifierExpandError::UnsupportedEndpoint { .. })
    ));
}
