use std::collections::{BTreeMap, BTreeSet};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};

use super::prepare;
use crate::node_graph::PrimitiveRegistry;

const PHYSICS_SOLIDS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/generator-presets/PhysicsSolids.json"
));
const UNIFORM_FORCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/scene-modifier-presets/UniformForce.json"
));

fn prepared_uniform_force() -> (
    EffectGraphDef,
    crate::node_graph::scene_modifier_expand::PreparedSceneModifierGraph,
) {
    let mut host: EffectGraphDef = serde_json::from_str(PHYSICS_SOLIDS).expect("physics fixture");
    let next_id = host.nodes.iter().map(|node| node.id).max().unwrap_or(0) + 1;
    let scene_id = host
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .expect("physics scene")
        .id;
    // The stock fixture uses all six render slots. Replace its last visible
    // body with liquid while retaining the targeted body's original slot.
    host.wires
        .retain(|wire| !(wire.to_node == scene_id && wire.to_port == "object_5"));
    host.nodes
        .push(node(next_id, "fluid_event_test", "node.fluid_surface"));
    host.nodes
        .push(node(next_id + 1, "fluid_event_object", "node.scene_object"));
    host.wires
        .push(wire(next_id, "vertices", next_id + 1, "vertices"));
    host.wires
        .push(wire(next_id + 1, "object", scene_id, "object_5"));
    let recipe: EffectGraphDef = serde_json::from_str(UNIFORM_FORCE).expect("force fixture");
    let instance = crate::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
        &host,
        &recipe,
        NodeId::new("route_uniform"),
        manifold_core::scene_modifier_preset::SceneNodeRef {
            scope: Vec::new(),
            node: NodeId::new("scene"),
        },
        manifold_core::scene_modifier_preset::SceneTargetSelection::Explicit {
            objects: vec![manifold_core::scene_modifier_preset::SceneNodeRef {
                scope: Vec::new(),
                node: NodeId::new("physics_demo_114"),
            }],
        },
    )
    .expect("force attaches");
    let owner = manifold_core::scene_modifier_edit::insert_scene_modifier(&host, 0, instance)
        .expect("force inserts")
        .graph;
    let prepared = crate::node_graph::scene_modifier_expand::prepare_scene_modifiers(
        &owner,
        &PrimitiveRegistry::with_builtin(),
    )
    .expect("force expands");
    (owner, prepared)
}

fn node(id: u32, node_id: &str, type_id: &str) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: NodeId::new(node_id),
        type_id: type_id.into(),
        handle: Some(node_id.into()),
        params: BTreeMap::new(),
        exposed_params: BTreeSet::new(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.into(),
        to_node,
        to_port: to_port.into(),
    }
}

fn graph() -> EffectGraphDef {
    EffectGraphDef {
        version: 1,
        name: Some("source test".into()),
        description: Some("layout only".into()),
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes: vec![
            node(1, "source", "node.value"),
            node(2, "fluid", "node.fluid_surface"),
            node(3, "material", "node.value"),
        ],
        wires: vec![wire(1, "out", 2, "fill_height")],
    }
}

fn coupled_graph(second_body: bool) -> EffectGraphDef {
    let mut def = EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes: vec![
            node(1, "scene", "node.render_scene"),
            node(2, "fluid", "node.fluid_surface"),
            node(3, "fluid_object", "node.scene_object"),
            node(4, "world", "node.physics_world"),
            node(5, "body", "node.rigid_body"),
            node(6, "body_object", "node.scene_object"),
        ],
        wires: vec![
            wire(2, "vertices", 3, "vertices"),
            wire(3, "object", 1, "object_0"),
            wire(5, "body", 4, "body_0"),
            wire(4, "pose_0", 6, "transform"),
            wire(6, "object", 1, "object_1"),
        ],
    };
    if second_body {
        def.nodes.push(node(7, "body2", "node.rigid_body"));
        def.nodes.push(node(8, "body2_object", "node.scene_object"));
        def.wires.extend([
            wire(7, "body", 4, "body_1"),
            wire(4, "pose_1", 8, "transform"),
            wire(8, "object", 1, "object_2"),
        ]);
    }
    def
}

fn string_metadata(
    bindings: Vec<serde_json::Value>,
) -> manifold_core::effect_graph_def::PresetMetadata {
    serde_json::from_value(serde_json::json!({
        "id": "physics",
        "displayName": "Physics",
        "category": "Diagnostic",
        "oscPrefix": "physics",
        "params": [],
        "bindings": [],
        "stringBindings": bindings,
    }))
    .expect("string metadata parses")
}

fn string_binding(id: &str, node_id: &str, param: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "label": id,
        "defaultValue": "asset.glb",
        "target": {"kind": "node", "nodeId": node_id, "param": param},
    })
}

fn digest(def: &EffectGraphDef) -> [u8; 32] {
    let registry = PrimitiveRegistry::with_builtin();
    let sources = prepare(def, def, &[], &registry).expect("source graph prepares");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].fluid, NodeId::new("fluid"));
    sources[0].digest
}

#[test]
fn asset_inventory_follows_fluid_and_coupled_ancestry_without_cache_or_appearance() {
    let registry = PrimitiveRegistry::with_builtin();
    let mut def = coupled_graph(false);
    def.nodes.extend([
        node(7, "geometry", "node.gltf_mesh_source"),
        node(8, "animation", "node.gltf_animation_source"),
        node(9, "appearance", "node.gltf_texture_source"),
        node(10, "role", "node.fluid_role_source"),
    ]);
    def.wires.extend([
        wire(7, "source", 5, "source"),
        wire(8, "translation_y", 2, "gravity"),
        wire(10, "role", 2, "role_0"),
    ]);
    let assets = |def: &EffectGraphDef| {
        prepare(def, def, &[], &registry)
            .unwrap()
            .remove(0)
            .asset_nodes
    };
    let expected: Vec<_> = ["animation", "body", "geometry", "role"]
        .into_iter()
        .map(NodeId::new)
        .collect();
    assert_eq!(assets(&def), expected);
    def.nodes.reverse();
    def.wires.reverse();
    assert_eq!(assets(&def), expected, "authored order is immaterial");
}

#[test]
fn asset_inventory_includes_event_only_ancestry() {
    let (owner, mut prepared) = prepared_uniform_force();
    let route = &prepared.impulse_routes[0];
    let field_output = prepared
        .def
        .nodes
        .iter()
        .find(|node| node.node_id == route.field_node)
        .unwrap()
        .id;
    let field = prepared
        .def
        .wires
        .iter()
        .find(|wire| wire.to_node == field_output && wire.to_port == "field")
        .unwrap()
        .from_node;
    let next_id = prepared.def.nodes.iter().map(|node| node.id).max().unwrap() + 1;
    prepared.def.nodes.push(node(
        next_id,
        "event_animation",
        "node.gltf_animation_source",
    ));
    prepared
        .def
        .wires
        .push(wire(next_id, "translation_y", field, "strength"));
    let sources = prepare(
        &prepared.def,
        &owner,
        &prepared.impulse_routes,
        &PrimitiveRegistry::with_builtin(),
    )
    .unwrap();
    assert!(
        sources[0]
            .asset_nodes
            .contains(&NodeId::new("event_animation"))
    );
}

#[test]
fn physics_inputs_change_identity_but_layout_and_unrelated_nodes_do_not() {
    let base = graph();
    let original = digest(&base);

    let mut layout = base.clone();
    layout.name = Some("renamed".into());
    layout.description = Some("different title".into());
    layout.nodes[0].handle = Some("moved source".into());
    layout.nodes[0].title = Some("Source title".into());
    layout.nodes[0].editor_pos = Some((91.0, -12.0));
    layout.nodes.swap(0, 2);
    layout.wires.reverse();
    assert_eq!(digest(&layout), original);

    let mut material = base.clone();
    material.nodes[2].params.insert(
        "colour".into(),
        manifold_core::effect_graph_def::SerializedParamValue::Float { value: 0.5 },
    );
    assert_eq!(digest(&material), original);

    let mut physics = base;
    physics.nodes[0].params.insert(
        "value".into(),
        manifold_core::effect_graph_def::SerializedParamValue::Float { value: 0.75 },
    );
    assert_ne!(digest(&physics), original);
}

#[test]
fn numeric_node_ids_do_not_participate_in_identity() {
    let base = graph();
    let original = digest(&base);
    let mut renumbered = base.clone();
    let numbers = [(1, 101), (2, 202), (3, 303)];
    for node in &mut renumbered.nodes {
        node.id = numbers
            .iter()
            .find_map(|(old, new)| (*old == node.id).then_some(*new))
            .expect("test node number");
    }
    for wire in &mut renumbered.wires {
        wire.from_node = numbers
            .iter()
            .find_map(|(old, new)| (*old == wire.from_node).then_some(*new))
            .expect("test wire source");
        wire.to_node = numbers
            .iter()
            .find_map(|(old, new)| (*old == wire.to_node).then_some(*new))
            .expect("test wire target");
    }
    assert_eq!(digest(&renumbered), original);
}

#[test]
fn string_targets_are_scoped_deduplicated_and_stable() {
    let mut base = graph();
    base.preset_metadata = Some(string_metadata(vec![
        string_binding("source_file", "source", "asset"),
        string_binding("source_alias", "source", "asset"),
        string_binding("fluid_file", "fluid", "surface_asset"),
        string_binding("fluid_cache", "fluid", "cache_path"),
        string_binding("appearance", "material", "texture"),
    ]));
    let registry = PrimitiveRegistry::with_builtin();
    let first = prepare(&base, &base, &[], &registry)
        .expect("string source graph")
        .pop()
        .expect("fluid source");
    assert_eq!(
        first.string_targets,
        vec![
            (NodeId::new("fluid"), "surface_asset".into()),
            (NodeId::new("source"), "asset".into()),
        ]
    );

    let mut reordered = base.clone();
    reordered
        .preset_metadata
        .as_mut()
        .expect("metadata")
        .string_bindings
        .reverse();
    let numbers = [(1, 101), (2, 202), (3, 303)];
    for node in &mut reordered.nodes {
        node.id = numbers
            .iter()
            .find_map(|(old, new)| (*old == node.id).then_some(*new))
            .expect("test node number");
    }
    for wire in &mut reordered.wires {
        wire.from_node = numbers
            .iter()
            .find_map(|(old, new)| (*old == wire.from_node).then_some(*new))
            .expect("test wire source");
        wire.to_node = numbers
            .iter()
            .find_map(|(old, new)| (*old == wire.to_node).then_some(*new))
            .expect("test wire target");
    }
    let stable = prepare(&reordered, &reordered, &[], &registry)
        .expect("reordered string source graph")
        .pop()
        .expect("fluid source");
    assert_eq!(stable.string_targets, first.string_targets);
}

#[test]
fn string_targets_include_coupled_rigid_ancestry() {
    let mut def = coupled_graph(false);
    def.preset_metadata = Some(string_metadata(vec![
        string_binding("rigid_file", "body", "collision_asset"),
        string_binding("rigid_alias", "body", "collision_asset"),
        string_binding("fluid_file", "fluid", "surface_asset"),
        string_binding("appearance_file", "body_object", "unrelated_asset"),
    ]));
    let registry = PrimitiveRegistry::with_builtin();
    let source = prepare(&def, &def, &[], &registry)
        .expect("coupled string source graph")
        .pop()
        .expect("fluid source");
    assert_eq!(
        source.string_targets,
        vec![
            (NodeId::new("body"), "collision_asset".into()),
            (NodeId::new("fluid"), "surface_asset".into()),
        ]
    );
}

#[test]
fn coupled_rigid_ancestry_and_body_mask_are_part_of_identity() {
    let one = coupled_graph(false);
    let two = coupled_graph(true);
    let registry = PrimitiveRegistry::with_builtin();
    let one_sources = prepare(&one, &one, &[], &registry).expect("one-body source graph");
    let two_sources = prepare(&two, &two, &[], &registry).expect("two-body source graph");
    assert_eq!(one_sources.len(), 1);
    assert_eq!(two_sources.len(), 1);
    assert_ne!(one_sources[0].digest, two_sources[0].digest);

    // Keep the complete world ancestry, changing only which body's visible
    // scene membership makes it a fluid collider.
    let mut masked = two.clone();
    masked
        .wires
        .retain(|wire| !(wire.from_node == 8 && wire.to_node == 1));
    let masked_sources = prepare(&masked, &masked, &[], &registry).unwrap();
    assert_ne!(two_sources[0].digest, masked_sources[0].digest);

    let mut copies = one.clone();
    copies
        .nodes
        .push(node(7, "copy_object", "node.scene_object"));
    copies.wires.extend([
        wire(5, "body", 4, "copies"),
        wire(4, "instances", 7, "instances"),
        wire(7, "object", 1, "object_2"),
    ]);
    let copies_sources = prepare(&copies, &copies, &[], &registry).unwrap();
    copies
        .wires
        .retain(|wire| !(wire.from_node == 7 && wire.to_node == 1));
    let hidden_copies_sources = prepare(&copies, &copies, &[], &registry).unwrap();
    assert_ne!(copies_sources[0].digest, hidden_copies_sources[0].digest);

    let mut rigid_edit = one.clone();
    rigid_edit.nodes[4].params.insert(
        "mass".into(),
        manifold_core::effect_graph_def::SerializedParamValue::Float { value: 4.0 },
    );
    let edited = prepare(&rigid_edit, &rigid_edit, &[], &registry).expect("rigid edit");
    assert_ne!(one_sources[0].digest, edited[0].digest);
}

#[test]
fn event_field_ancestry_and_expanded_binding_semantics_are_hashed() {
    let (owner, prepared) = prepared_uniform_force();
    assert!(
        !prepared.impulse_routes.is_empty(),
        "force fixture has an event route"
    );
    let registry = PrimitiveRegistry::with_builtin();
    let base = prepare(&prepared.def, &owner, &prepared.impulse_routes, &registry)
        .expect("event source graph");
    assert!(!base.is_empty());

    let route = &prepared.impulse_routes[0];
    let output = prepared
        .def
        .nodes
        .iter()
        .find(|node| node.node_id == route.field_node)
        .expect("generated event output");
    let strength_number = prepared
        .def
        .wires
        .iter()
        .find(|wire| wire.to_node == output.id && wire.to_port == "field")
        .expect("event-only strength input")
        .from_node;
    let mut field_edit = prepared.def.clone();
    let field = field_edit
        .nodes
        .iter_mut()
        .find(|node| node.id == strength_number)
        .expect("event-only strength node");
    let strength_id = field.node_id.clone();
    field.params.insert(
        "strength".into(),
        manifold_core::effect_graph_def::SerializedParamValue::Float { value: 5.0 },
    );
    let changed_field = prepare(&field_edit, &owner, &prepared.impulse_routes, &registry)
        .expect("edited event source graph");
    assert_ne!(base[0].digest, changed_field[0].digest);

    let mut binding_edit = prepared.def.clone();
    let metadata = binding_edit
        .preset_metadata
        .as_mut()
        .expect("expanded metadata");
    metadata.bindings.push(
        serde_json::from_value(serde_json::json!({
            "id": "generated_force",
            "label": "Force",
            "defaultValue": 0.5,
            "target": {"kind": "node", "nodeId": strength_id, "param": "strength"},
            "scale": 1.0,
            "offset": 0.0
        }))
        .expect("generated binding"),
    );
    metadata.params.push(
        serde_json::from_value(serde_json::json!({
            "id": "generated_force",
            "name": "Force",
            "min": 0.0,
            "max": 1.0,
            "defaultValue": 0.5
        }))
        .expect("generated spec"),
    );
    let changed_binding = prepare(&binding_edit, &owner, &prepared.impulse_routes, &registry)
        .expect("binding source graph");
    assert_ne!(base[0].digest, changed_binding[0].digest);
    assert!(
        changed_binding[0]
            .control_ids
            .iter()
            .any(|id| id == "generated_force")
    );

    let mut reshaped = binding_edit;
    reshaped
        .preset_metadata
        .as_mut()
        .expect("expanded metadata")
        .bindings
        .last_mut()
        .expect("generated binding")
        .offset = 0.25;
    let changed_reshape = prepare(&reshaped, &owner, &prepared.impulse_routes, &registry)
        .expect("reshaped source graph");
    assert_ne!(changed_binding[0].digest, changed_reshape[0].digest);

    let mut alias_owner = owner.clone();
    {
        let alias_metadata = alias_owner
            .preset_metadata
            .as_mut()
            .expect("canonical metadata");
        alias_metadata.bindings.push(
            serde_json::from_value(serde_json::json!({
                "id": "force_trigger_alias",
                "label": "Trigger",
                "defaultValue": 0.0,
                "target": {
                        "kind": "sceneModifier",
                    "modifierId": route.modifier_id.as_str(),
                    "paramId": route.param_id.as_str()
                }
            }))
            .expect("event alias"),
        );
        alias_metadata.params.push(
            serde_json::from_value(serde_json::json!({
                "id": "force_trigger_alias",
                "name": "Trigger",
                "min": 0.0,
                "max": 1.0,
                "defaultValue": 0.0,
                "isTrigger": true
            }))
            .expect("event alias spec"),
        );
    }
    let alias_digest = prepare(
        &prepared.def,
        &alias_owner,
        &prepared.impulse_routes,
        &registry,
    )
    .expect("event alias source graph");
    assert_ne!(base[0].digest, alias_digest[0].digest);
    assert!(
        alias_digest[0]
            .control_ids
            .iter()
            .any(|id| id == "force_trigger_alias")
    );
    assert!(
        alias_digest[0]
            .control_ids
            .iter()
            .all(|id| id != "cache_mode")
    );

    alias_owner
        .preset_metadata
        .as_mut()
        .expect("canonical metadata")
        .params
        .last_mut()
        .expect("event alias spec")
        .max = 2.0;
    let reshaped_alias = prepare(
        &prepared.def,
        &alias_owner,
        &prepared.impulse_routes,
        &registry,
    )
    .expect("reshaped event alias source graph");
    assert_ne!(alias_digest[0].digest, reshaped_alias[0].digest);

    let mut event_string_def = prepared.def.clone();
    event_string_def
        .preset_metadata
        .as_mut()
        .expect("expanded metadata")
        .string_bindings
        .push(
            serde_json::from_value(string_binding(
                "event_field_asset",
                route.field_node.as_str(),
                "field_asset",
            ))
            .expect("event string binding"),
        );
    let event_string_sources = prepare(
        &event_string_def,
        &owner,
        &prepared.impulse_routes,
        &registry,
    )
    .expect("event field string source graph");
    assert!(
        event_string_sources[0]
            .string_targets
            .contains(&(route.field_node.clone(), "field_asset".into()))
    );
}

#[test]
fn legacy_handle_identity_is_normalized_and_anonymous_fluid_is_rejected() {
    let base = graph();
    let original = digest(&base);
    let mut legacy = base.clone();
    for node in &mut legacy.nodes {
        node.node_id = NodeId::default();
    }
    assert_eq!(digest(&legacy), original);

    let mut anonymous = base;
    anonymous.nodes[1].node_id = NodeId::default();
    anonymous.nodes[1].handle = None;
    let registry = PrimitiveRegistry::with_builtin();
    let error = prepare(&anonymous, &anonymous, &[], &registry)
        .err()
        .expect("anonymous fluid");
    assert!(error.contains("recording provenance is unsupported"));
}

#[test]
fn cache_controls_do_not_change_identity() {
    let mut first = graph();
    first.nodes[1].params.insert(
        "cache_mode".into(),
        manifold_core::effect_graph_def::SerializedParamValue::Enum { value: 0 },
    );
    first.nodes[1].params.insert(
        "cache_path".into(),
        manifold_core::effect_graph_def::SerializedParamValue::String { value: "a".into() },
    );
    first.nodes[1].params.insert(
        "reset".into(),
        manifold_core::effect_graph_def::SerializedParamValue::Float { value: 0.0 },
    );
    let mut second = first.clone();
    second.nodes[1].params.insert(
        "cache_mode".into(),
        manifold_core::effect_graph_def::SerializedParamValue::Enum { value: 2 },
    );
    second.nodes[1].params.insert(
        "cache_path".into(),
        manifold_core::effect_graph_def::SerializedParamValue::String { value: "b".into() },
    );
    second.nodes[1].params.insert(
        "reset".into(),
        manifold_core::effect_graph_def::SerializedParamValue::Float { value: 1.0 },
    );
    assert_eq!(digest(&first), digest(&second));
}

#[test]
fn relevant_binding_reshape_and_asset_selector_change_identity() {
    let mut first = graph();
    first.preset_metadata = Some(
        serde_json::from_value(serde_json::json!({
            "id": "physics",
            "displayName": "Physics",
            "category": "Diagnostic",
            "oscPrefix": "physics",
            "params": [],
            "bindings": [{
                "id": "fill",
                "label": "Fill",
                "defaultValue": 0.5,
                "target": {"kind": "node", "nodeId": "fluid", "param": "fill_height"},
                "scale": 1.0,
                "offset": 0.0
            }, {
                "id": "cache",
                "label": "Cache",
                "defaultValue": 0.0,
                "target": {"kind": "node", "nodeId": "fluid", "param": "cache_mode"},
                "scale": 1.0,
                "offset": 0.0
            }],
            "stringBindings": [{
                "id": "asset",
                "label": "Asset",
                "defaultValue": "a.glb",
                "target": {"kind": "node", "nodeId": "source", "param": "asset"}
            }]
        }))
        .expect("metadata parses"),
    );
    let mut reshape = first.clone();
    reshape.preset_metadata.as_mut().expect("metadata").bindings[0].offset = 0.25;
    assert_ne!(digest(&first), digest(&reshape));

    let mut cache_binding = first.clone();
    cache_binding
        .preset_metadata
        .as_mut()
        .expect("metadata")
        .bindings[1]
        .offset = 1.0;
    assert_eq!(digest(&first), digest(&cache_binding));

    let mut asset = first.clone();
    asset
        .preset_metadata
        .as_mut()
        .expect("metadata")
        .string_bindings[0]
        .default_value = "b.glb".into();
    assert_ne!(digest(&first), digest(&asset));

    let registry = PrimitiveRegistry::with_builtin();
    let first_source = prepare(&first, &first, &[], &registry)
        .expect("control identity")
        .pop()
        .expect("fluid source");
    assert_eq!(first_source.control_ids, vec!["fill"]);

    let mut ordered = first.clone();
    let ordered_metadata = ordered.preset_metadata.as_mut().expect("metadata");
    ordered_metadata.bindings.push(
        serde_json::from_value(serde_json::json!({
            "id": "zeta",
            "label": "Zeta",
            "defaultValue": 0.0,
            "target": {"kind": "node", "nodeId": "source", "param": "value"}
        }))
        .expect("ordered binding"),
    );
    ordered_metadata.bindings.push(
        serde_json::from_value(serde_json::json!({
            "id": "fill",
            "label": "Fill duplicate",
            "defaultValue": 0.5,
            "target": {"kind": "node", "nodeId": "fluid", "param": "fill_height"}
        }))
        .expect("duplicate binding"),
    );
    let ordered_source = prepare(&ordered, &ordered, &[], &registry)
        .expect("ordered controls")
        .pop()
        .expect("fluid source");
    assert_eq!(ordered_source.control_ids, vec!["fill", "zeta"]);

    let mut spec_edit = first.clone();
    spec_edit
        .preset_metadata
        .as_mut()
        .expect("metadata")
        .params
        .push(
            serde_json::from_value(serde_json::json!({
                "id": "fill",
                "name": "Fill",
                "min": 0.0,
                "max": 1.0,
                "defaultValue": 0.5,
                "isTriggerGate": true,
                "valueLabels": ["Low", "High"]
            }))
            .expect("semantic spec"),
        );
    let spec_digest = digest(&spec_edit);
    let mut label_edit = spec_edit.clone();
    label_edit
        .preset_metadata
        .as_mut()
        .expect("metadata")
        .params
        .last_mut()
        .expect("semantic spec")
        .value_labels = vec!["Quiet".into(), "Loud".into()];
    assert_eq!(spec_digest, digest(&label_edit));
    label_edit
        .preset_metadata
        .as_mut()
        .unwrap()
        .params
        .last_mut()
        .unwrap()
        .value_labels
        .clear();
    assert_ne!(spec_digest, digest(&label_edit));
    let before_gate = digest(&label_edit);
    label_edit
        .preset_metadata
        .as_mut()
        .unwrap()
        .params
        .last_mut()
        .unwrap()
        .is_trigger_gate = false;
    assert_ne!(before_gate, digest(&label_edit));
}

#[test]
fn nonphysics_graphs_are_not_rejected_for_unrelated_bad_wires() {
    let mut def = graph();
    def.nodes[1].type_id = "node.value".into();
    def.wires.push(wire(900, "out", 1, "in"));
    let registry = PrimitiveRegistry::with_builtin();
    assert!(prepare(&def, &def, &[], &registry).unwrap().is_empty());
}
