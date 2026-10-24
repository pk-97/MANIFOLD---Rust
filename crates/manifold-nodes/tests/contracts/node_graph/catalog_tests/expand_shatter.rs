use manifold_core::effect_graph_def::*;
use manifold_core::scene_modifier_preset::SceneTargetSelection;

fn fixture() -> EffectGraphDef {
    let mut owner: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/presets/PhysicsSolids.json"
    )))
    .unwrap();
    owner.version = 3;
    let mesh = owner.nodes.iter_mut().find(|n| n.id == 112).unwrap();
    mesh.type_id = "node.gltf_mesh_source".into();
    mesh.params.clear();
    mesh.params.insert(
        "path".into(),
        SerializedParamValue::String {
            value: "scan.glb".into(),
        },
    );
    mesh.params.insert(
        "max_capacity".into(),
        SerializedParamValue::Int { value: 300 },
    );
    mesh.params.insert(
        "source_vertex_count".into(),
        SerializedParamValue::Int { value: 300 },
    );
    mesh.params.insert("source_bbox_radius".into(), float(1.0));
    owner.wires.retain(|w| w.to_node != 112);
    let parent = owner.nodes.iter_mut().find(|n| n.id == 111).unwrap();
    parent.params.insert(
        "path".into(),
        SerializedParamValue::String {
            value: "scan.glb".into(),
        },
    );
    let recipe = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/scene-modifier-presets/Shatter.json"
    )))
    .unwrap();
    let mut modifier = SceneModifierInstanceDef {
        id: NodeId::new("shatter-test"),
        scene: NodeId::new("scene"),
        targets: SceneTargetSelection::Explicit {
            objects: vec![NodeId::new("physics_demo_114")],
        },
        mesh_frames: vec![],
        legacy_math_view_carrier: None,
        graph: Box::new(recipe),
    };
    modifier.mesh_frames = resolve_modifier_mesh_frames(&owner, &modifier).unwrap();
    owner.scene_modifiers.push(modifier);
    owner
}

#[test]
fn shatter_expands_only_runtime_and_preserves_material_and_live_property_routes() {
    let owner = fixture();
    let saved = owner.clone();
    let prepared = prepare_scene_modifiers(&owner, &PrimitiveRegistry::with_builtin()).unwrap();
    assert_eq!(owner, saved);
    let fragments: Vec<_> = prepared
        .def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.rigid_body" && n.params.contains_key("fragment_parent"))
        .collect();
    assert_eq!(fragments.len(), 16);
    assert!(
        fragments
            .iter()
            .all(|n| number(n.params.get("fragment_parent")) == Some(1.0))
    );
    let material = prepared
        .def
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "physics_demo_113")
        .unwrap()
        .id;
    let material_uses = prepared
        .def
        .wires
        .iter()
        .filter(|w| w.from_node == material && w.to_port == "material")
        .count();
    assert_eq!(
        material_uses, 17,
        "original plus internal draws share the same material"
    );
    assert_eq!(
        prepared
            .def
            .preset_metadata
            .as_ref()
            .unwrap()
            .bindings
            .len(),
        prepared.binding_sources.len()
    );
    let graph = prepared
        .def
        .clone()
        .into_graph(&PrimitiveRegistry::with_builtin(), &Default::default())
        .unwrap();
    manifold_node_engine::load::expand::PreparedGraphValueWrites::prepare(
        &owner,
        &prepared.routes,
        &graph,
        &Default::default(),
    )
    .unwrap();
}

#[test]
fn shatter_inherits_parent_mass_or_registered_default() {
    let registry = PrimitiveRegistry::with_builtin();
    let body = registry.construct("node.rigid_body").unwrap();
    let default = body
        .parameters()
        .iter()
        .find(|param| param.name == "mass")
        .unwrap()
        .default
        .as_scalar()
        .unwrap();
    assert_eq!(default, 600.0);
    for (authored, expected) in [(None, default), (Some(float(1234.0)), 1234.0)] {
        let mut owner = fixture();
        let parent = owner.nodes.iter_mut().find(|node| node.id == 111).unwrap();
        parent.params.remove("mass");
        if let Some(value) = authored {
            parent.params.insert("mass".into(), value);
        }
        let prepared = prepare_scene_modifiers(&owner, &registry).unwrap();
        let fragments: Vec<_> = prepared
            .def
            .nodes
            .iter()
            .filter(|node| node.params.contains_key("fragment_parent"))
            .collect();
        assert_eq!(fragments.len(), 16);
        for fragment in fragments {
            assert_eq!(number(fragment.params.get("mass")), Some(expected));
        }
    }
}

#[test]
fn shatter_fans_parent_acceleration_to_every_fragment() {
    let mut owner = fixture();
    owner.nodes.push(
        serde_json::from_value(serde_json::json!({
            "id": 900,
            "nodeId": "shatter-field",
            "typeId": "node.uniform_vector_field"
        }))
        .unwrap(),
    );
    owner.wires.push(EffectGraphWire {
        from_node: 900,
        from_port: "out".into(),
        to_node: 111,
        to_port: "acceleration".into(),
    });

    let prepared = prepare_scene_modifiers(&owner, &PrimitiveRegistry::with_builtin()).unwrap();
    let prepared_id = |stable: &NodeId| {
        prepared
            .def
            .nodes
            .iter()
            .find(|node| &node.node_id == stable)
            .unwrap()
            .id
    };
    let world = prepared_id(
        &owner
            .nodes
            .iter()
            .find(|node| node.id == 40)
            .unwrap()
            .node_id,
    );
    let field = prepared_id(&NodeId::new("shatter-field"));
    let parent = prepared_id(
        &owner
            .nodes
            .iter()
            .find(|node| node.id == 111)
            .unwrap()
            .node_id,
    );
    let fragment_ids: BTreeSet<_> = prepared
        .def
        .nodes
        .iter()
        .filter(|node| {
            node.type_id == "node.rigid_body" && node.params.contains_key("fragment_parent")
        })
        .map(|node| node.id)
        .collect();
    let fragment_slots: BTreeSet<_> = prepared
        .def
        .wires
        .iter()
        .filter(|wire| fragment_ids.contains(&wire.from_node) && wire.to_node == world)
        .filter_map(
            |wire| match manifold_core::physics_members::member_input(&wire.to_port) {
                Some((manifold_core::physics_members::Family::Body, slot)) => Some(slot),
                _ => None,
            },
        )
        .collect();
    assert_eq!(fragment_ids.len(), 16);
    assert_eq!(
        fragment_slots,
        (6..22).collect(),
        "fragments take the world's free slots after the six authored bodies"
    );
    for fragment in &fragment_ids {
        assert!(
            prepared.def.wires.iter().any(|wire| {
                wire.from_node == field
                    && wire.from_port == "out"
                    && wire.to_node == *fragment
                    && wire.to_port == "acceleration"
            }),
            "fragment {fragment} takes the parent's force on its own input"
        );
    }
    let field_wires: Vec<_> = prepared
        .def
        .wires
        .iter()
        .filter(|wire| wire.from_node == field && wire.from_port == "out")
        .collect();
    assert_eq!(
        field_wires.len(),
        17,
        "the parent's route and one per fragment"
    );
    assert_eq!(
        field_wires
            .iter()
            .filter(|wire| wire.to_node == parent)
            .count(),
        1,
        "the parent route remains a single authored connection"
    );
    let fragment_poses = prepared
        .def
        .wires
        .iter()
        .filter(|wire| fragment_ids.contains(&wire.from_node) && wire.from_port == "pose")
        .count();
    assert!(
        fragment_poses >= 16,
        "each fragment's object reads its own body's pose: {fragment_poses}"
    );
    assert!(
        !prepared
            .def
            .wires
            .iter()
            .any(|wire| wire.from_node == field && wire.to_port == "acceleration_field")
    );
}

#[test]
fn shatter_disabled_keeps_intact_graph_and_duplicate_is_rejected() {
    let mut owner = fixture();
    let mut disabled = owner.clone();
    let meta = disabled.scene_modifiers[0]
        .graph
        .preset_metadata
        .as_mut()
        .unwrap();
    meta.bindings
        .iter_mut()
        .find(|b| b.id == "enabled")
        .unwrap()
        .default_value = 0.0;
    meta.params
        .iter_mut()
        .find(|p| p.id == "enabled")
        .unwrap()
        .default_value = 0.0;
    let prepared = prepare_scene_modifiers(&disabled, &PrimitiveRegistry::with_builtin()).unwrap();
    assert!(
        !prepared
            .def
            .nodes
            .iter()
            .any(|n| n.params.contains_key("fragment_parent"))
    );
    let mut second = owner.scene_modifiers[0].clone();
    second.id = NodeId::new("second-shatter");
    owner.scene_modifiers.push(second);
    assert!(
        prepare_scene_modifiers(&owner, &PrimitiveRegistry::with_builtin())
            .unwrap_err()
            .to_string()
            .contains("one active Shatter")
    );
}

use manifold_core::{NodeId, scene_modifier_preset::SceneModifierInstanceDef};
use manifold_node_engine::load::expand::{prepare_scene_modifiers, resolve_modifier_mesh_frames};
use manifold_node_engine::persistence::PrimitiveRegistry;
use std::collections::BTreeSet;

use manifold_node_engine::persistence::EffectGraphDefExt;

fn float(value: f32) -> SerializedParamValue {
    SerializedParamValue::Float { value }
}
fn number(value: Option<&SerializedParamValue>) -> Option<f32> {
    match value {
        Some(SerializedParamValue::Float { value }) => Some(*value),
        Some(SerializedParamValue::Int { value }) => Some(*value as f32),
        Some(SerializedParamValue::Enum { value }) => Some(*value as f32),
        Some(SerializedParamValue::Bool { value }) => Some(u8::from(*value) as f32),
        _ => None,
    }
}
