//! Focused structural coverage for the bundled Box3D Physics Solids scene.
//!
//! The runtime and renderer proofs cover simulation and drawing. These tests
//! keep the authored graph's scene-panel discovery contract explicit: every
//! visible object has an editable starting transform/material, every body is
//! paired with the shared world, and the load-time exposure migration remains
//! complete and idempotent.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, SerializedParamValue,
};
use manifold_core::effects::ParamConvert;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::node_graph::scene_exposure::migrate_scene_exposures;
use manifold_renderer::node_graph::scene_vm::{
    MaterialVm, SceneObjectVm, SceneVm, physics_body_doc_id, physics_world_doc_ids,
};
use manifold_renderer::preset_runtime::PresetRuntime;

const PHYSICS_SOLIDS_JSON: &str = include_str!("../assets/generator-presets/PhysicsSolids.json");
const PHYSICS_BOXES_JSON: &str = include_str!("../assets/generator-presets/PhysicsBoxes.json");

#[test]
fn physics_boxes_compiles_with_count_reset_and_shared_floor() {
    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_json_str(PHYSICS_BOXES_JSON, &registry)
        .expect("PhysicsBoxes must compile through the production loader");
    let mut def: EffectGraphDef = serde_json::from_str(PHYSICS_BOXES_JSON).unwrap();
    migrate_scene_exposures(&mut def);
    let vm = SceneVm::from_def(&def).expect("box demo is a scene");
    assert_eq!(vm.objects.len(), 4);
    let metadata = def.preset_metadata.as_ref().unwrap();
    let count = metadata
        .params
        .iter()
        .find(|p| p.id == "40_copy_count")
        .unwrap();
    assert_eq!(
        (count.min, count.max, count.default_value),
        (0.0, 4096.0, 256.0)
    );
    assert!(count.whole_numbers && count.card_visible);
    assert!(
        metadata
            .params
            .iter()
            .find(|p| p.id == "40_reset")
            .unwrap()
            .is_trigger
    );
    for (from, port, to, input) in [
        (101, "body", 40, "body_0"),
        (141, "body", 40, "body_1"),
        (161, "body", 40, "body_2"),
        (121, "body", 40, "copies"),
        (40, "instances", 124, "instances"),
        (40, "active_count", 124, "instance_count"),
    ] {
        assert!(def.wires.iter().any(|w| w.from_node == from
            && w.from_port == port
            && w.to_node == to
            && w.to_port == input));
    }
    let roundtrip: EffectGraphDef =
        serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
    assert_eq!(def, roundtrip);
}

#[test]
fn physics_boxes_contacts_deflect_the_pile_sideways() {
    use manifold_core::Seconds;
    use manifold_renderer::node_graph::physics::{MAX_BODIES, RigidBody, RigidSimulation};
    use manifold_renderer::node_graph::transform::Transform;

    let def: EffectGraphDef = serde_json::from_str(PHYSICS_BOXES_JSON).unwrap();
    let nodes = nodes_by_id(&def);
    let scalar = |id: u32, name: &str| match nodes[&id].params.get(name).unwrap() {
        SerializedParamValue::Float { value } => *value,
        SerializedParamValue::Enum { value } => *value as f32,
        other => panic!("unexpected {name} value: {other:?}"),
    };
    let body = |id: u32| RigidBody {
        transform: Transform {
            pos: ["pos_x", "pos_y", "pos_z"].map(|p| scalar(id - 1, p)),
            rot_euler: ["rot_x", "rot_y", "rot_z"].map(|p| scalar(id - 1, p)),
            scale: ["scale_x", "scale_y", "scale_z"].map(|p| scalar(id - 1, p)),
            billboard: false,
        },
        shape: scalar(id, "shape") as u32,
        kind: scalar(id, "motion") as u32,
        mass: scalar(id, "mass"),
        friction: scalar(id, "friction"),
        bounce: scalar(id, "bounce"),
    };
    let mut bodies = [None; MAX_BODIES];
    for (slot, id) in [101, 141, 161].into_iter().enumerate() {
        bodies[slot] = Some(body(id));
    }
    let prototype = Some(body(121));
    let mut sim = RigidSimulation::default();
    let advance = |sim: &mut RigidSimulation, frame| {
        sim.advance_with_copy_layout(
            bodies,
            prototype,
            scalar(40, "copy_count"),
            scalar(40, "copy_spacing"),
            scalar(40, "copy_columns"),
            scalar(40, "copy_layout"),
            [0.0, -9.81, 0.0],
            Seconds(f64::from(frame) / 60.0),
            1.0,
            0.0,
        )
        .unwrap();
    };
    advance(&mut sim, 0);
    let initial = sim.copy_poses[..sim.active_copy_count].to_vec();
    for frame in 1..=240 {
        advance(&mut sim, frame);
    }
    let deflected = initial
        .iter()
        .zip(&sim.copy_poses)
        .filter(|(a, b)| {
            let dx = b.pos[0] - a.pos[0];
            let dz = b.pos[2] - a.pos[2];
            dx * dx + dz * dz > 0.25
        })
        .count();
    assert!(
        deflected > initial.len() / 4,
        "contacts must scatter boxes, not just drop an unchanged grid: {deflected}"
    );
    assert!(
        sim.copy_poses[..sim.active_copy_count]
            .iter()
            .all(|pose| pose.pos.iter().all(|v| v.is_finite()) && pose.pos[1] > -1.0)
    );
}

fn parse_preset() -> EffectGraphDef {
    serde_json::from_str(PHYSICS_SOLIDS_JSON).expect("PhysicsSolids preset must parse")
}

fn nodes_by_id(def: &EffectGraphDef) -> BTreeMap<u32, &EffectGraphNode> {
    def.nodes.iter().map(|node| (node.id, node)).collect()
}

fn binding_targets(def: &EffectGraphDef) -> BTreeSet<(u32, String)> {
    def.preset_metadata
        .as_ref()
        .expect("scene exposure migration creates preset metadata")
        .bindings
        .iter()
        .filter_map(|binding| match &binding.target {
            BindingTarget::Node { node_id, param } => Some((
                def.nodes
                    .iter()
                    .find(|node| node.node_id.as_str() == node_id.as_str())
                    .map(|node| node.id)
                    .unwrap_or_else(|| {
                        panic!("binding {} targets missing node {node_id}", binding.id)
                    }),
                param.clone(),
            )),
            _ => None,
        })
        .collect()
}

#[test]
fn physics_solids_compiles_and_scene_objects_resolve_editable_sources() {
    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_json_str(PHYSICS_SOLIDS_JSON, &registry)
        .expect("PhysicsSolids must compile through the persistence loader");

    let def = parse_preset();
    let vm = SceneVm::from_def(&def).expect("PhysicsSolids must resolve as a scene");
    assert_eq!(vm.objects.len(), 6, "six solids are authored in the scene");

    let expected_objects = [
        (104, 100, 103),
        (114, 110, 113),
        (124, 120, 123),
        (134, 130, 133),
        (144, 140, 143),
        (154, 150, 153),
    ];
    for (object, (object_id, transform_id, material_id)) in vm.objects.iter().zip(expected_objects)
    {
        let SceneObjectVm::Known(row) = object else {
            panic!("PhysicsSolids object must resolve to a known scene object");
        };
        assert_eq!(row.object_node_id, object_id);
        assert_eq!(
            row.transform
                .as_ref()
                .map(|transform| transform.node_doc_id),
            Some(transform_id),
            "starting transform must be discoverable for object {object_id}"
        );
        match &row.material {
            MaterialVm::Known(material) => assert_eq!(material.node_doc_id, material_id),
            MaterialVm::None => panic!("object {object_id} must resolve its PBR material"),
        }
        assert!(
            !row.transform_chain_parseable,
            "modifiers must not splice across the solver"
        );
        assert!(row.modifier_chain_parseable);
    }

    assert_eq!(
        physics_world_doc_ids(&def).collect::<Vec<_>>(),
        vec![40],
        "all six bodies share world node 40"
    );
    for (object_id, body_id) in [
        (104, 101),
        (114, 111),
        (124, 121),
        (134, 131),
        (144, 141),
        (154, 151),
    ] {
        assert_eq!(
            physics_body_doc_id(&def, object_id),
            Some(body_id),
            "object {object_id} must resolve its authored rigid body"
        );
    }
}

#[test]
fn physics_solids_exposures_cover_body_world_triggers_and_roundtrip() {
    let mut migrated = parse_preset();
    assert!(migrate_scene_exposures(&mut migrated));

    let targets = binding_targets(&migrated);
    for body_id in [101, 111, 121, 131, 141, 151] {
        for param in ["mass", "friction", "bounce", "motion", "shape"] {
            assert!(
                targets.contains(&(body_id, param.to_string())),
                "body {body_id}.{param} must be exposed"
            );
        }
    }
    for param in ["gravity_x", "gravity_y", "gravity_z", "speed", "reset"] {
        assert!(
            targets.contains(&(40, param.to_string())),
            "world 40.{param} must be exposed"
        );
    }

    let metadata = migrated.preset_metadata.as_ref().expect("metadata");
    let reset_specs: Vec<_> = metadata
        .params
        .iter()
        .filter(|spec| spec.name == "Reset")
        .collect();
    assert_eq!(reset_specs.len(), 1, "world reset has one scene exposure");
    assert!(reset_specs[0].is_trigger, "reset retains trigger metadata");
    let reset_binding = metadata
        .bindings
        .iter()
        .find(|binding| {
            matches!(
                &binding.target,
                BindingTarget::Node { node_id, param }
                    if node_id.as_str() == "physics_demo_40" && param == "reset"
            )
        })
        .expect("world reset binding");
    assert_eq!(reset_binding.convert, ParamConvert::Trigger);

    let once = migrated.clone();
    assert!(
        !migrate_scene_exposures(&mut migrated),
        "migration must be idempotent"
    );
    assert_eq!(migrated, once);

    let serialized = serde_json::to_string(&migrated).expect("migrated preset serializes");
    let roundtrip: EffectGraphDef =
        serde_json::from_str(&serialized).expect("serialized PhysicsSolids reloads");
    assert_eq!(
        roundtrip, migrated,
        "exposure metadata must round-trip exactly"
    );

    // Keep this helper exercised against the actual node params as well as
    // the binding surface: all six body descriptions and the world are
    // ordinary nodes in the persisted graph.
    let nodes = nodes_by_id(&roundtrip);
    for id in [101, 111, 121, 131, 141, 151] {
        assert_eq!(nodes[&id].type_id, "node.rigid_body");
        assert!(matches!(
            nodes[&id].params.get("mass"),
            Some(SerializedParamValue::Float { .. })
        ));
    }
    assert_eq!(nodes[&40].type_id, "node.physics_world");
}
