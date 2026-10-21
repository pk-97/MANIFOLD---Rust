//! Focused structural coverage for the bundled Box3D Physics Solids scene.
//!
//! The runtime and renderer proofs cover simulation and drawing. These tests
//! keep the authored graph's scene-panel discovery contract explicit: every
//! visible object has an editable starting transform/material, every body is
//! paired with the shared world, and the load-time exposure migration remains
//! complete and idempotent.

use manifold_nodes as _;
use std::collections::{BTreeMap, BTreeSet};

use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, SerializedParamValue,
};
use manifold_core::effects::ParamConvert;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_nodes_scene::node_graph::scene_exposure::migrate_scene_exposures;
use manifold_nodes_scene::node_graph::scene_vm::{MaterialVm, SceneObjectVm, SceneVm};
use manifold_node_engine::runtime::PresetRuntime;

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
        (0.0, 4000.0, 256.0)
    );
    assert!(count.whole_numbers && count.card_visible);
    // The box spawner is the copies body's: all four stamp there, none on
    // the world or on the bodies that are not copied.
    let targets = binding_targets(&def);
    for param in ["copy_count", "copy_spacing", "copy_columns", "copy_layout"] {
        assert!(targets.contains(&(121, param.to_string())), "copies body 121.{param} must be exposed");
    }
    assert_eq!(
        targets.iter().filter(|(_, param)| param.starts_with("copy_")).count(),
        4,
        "{targets:?}"
    );
    // One id plan: the controls keep the ids they had on the world, so a
    // saved value and anything attached to it still land, and the stamp adds
    // no duplicate under the body's own doc id.
    for id in ["40_copy_count", "40_copy_spacing", "40_copy_columns", "40_copy_layout"] {
        assert!(metadata.params.iter().any(|p| p.id == id), "{id}");
    }
    assert!(!metadata.params.iter().any(|p| p.id.starts_with("121_copy_")));
    assert!(metadata.param_aliases.is_empty(), "no renames: {:?}", metadata.param_aliases);
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
    use manifold_core::PlayedTime;
    use manifold_water_rigid::physics::{MAX_BODIES, RigidBody, RigidSimulation};
    use manifold_node_engine::scene::transform::Transform;

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
        enabled: true,
        shape: scalar(id, "shape") as u32,
        kind: scalar(id, "motion") as u32,
        density: scalar(id, "density"),
        friction: scalar(id, "friction"),
        bounce: scalar(id, "bounce"),
        collider: None,
        fragment_parent: None,
        release_count: 0.0,
        wall: false,
        spawn: Default::default(),
    };
    let mut bodies: [Option<RigidBody>; MAX_BODIES] = std::array::from_fn(|_| None);
    for (slot, id) in [101, 141, 161].into_iter().enumerate() {
        bodies[slot] = Some(body(id));
    }
    let prototype = Some(body(121));
    let mut sim = RigidSimulation::default();
    let advance = |sim: &mut RigidSimulation, frame| {
        sim.advance_with_copy_layout(
            bodies.clone(),
            prototype.clone(),
            scalar(121, "copy_count"),
            scalar(121, "copy_spacing"),
            scalar(121, "copy_columns"),
            scalar(121, "copy_layout"),
            [0.0, -9.81, 0.0],
            PlayedTime::from_played(f64::from(frame) / 60.0),
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

    let world = def.nodes.iter().find(|node| node.id == 40).expect("world node 40");
    assert_eq!(world.type_id, "node.physics_world");
    assert!(
        manifold_nodes_scene::node_graph::scene_items::scene_items(&def, &vm).iter().any(|item| item.kind == manifold_core::scene_surface::SceneItemKind::Physics && item.owned_nodes.contains(&world.node_id)),
        "all six bodies share world node 40, and its physics item owns its controls"
    );
    for (object, body_id) in vm.objects.iter().zip([101, 111, 121, 131, 141, 151]) {
        let SceneObjectVm::Known(row) = object else { unreachable!("checked above") };
        let physics = row.physics.as_ref().expect("an authored rigid body");
        let body = def.nodes.iter().find(|node| node.id == body_id).unwrap();
        assert_eq!(
            (physics.body_node_id, &physics.body),
            (body_id, &body.node_id),
            "object {} must resolve its authored rigid body",
            row.object_node_id
        );
    }
}

#[test]
fn physics_solids_exposures_cover_body_world_triggers_and_roundtrip() {
    let mut migrated = parse_preset();
    assert!(migrate_scene_exposures(&mut migrated));

    let targets = binding_targets(&migrated);
    for body_id in [101, 111, 121, 131, 141, 151] {
        for param in ["density", "friction", "bounce", "motion", "shape"] {
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
    // No copies body, so no box sliders anywhere.
    assert!(
        !targets.iter().any(|(_, param)| param.starts_with("copy_")),
        "a scene without boxes shows no box controls"
    );

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
            nodes[&id].params.get("density"),
            Some(SerializedParamValue::Float { .. })
        ));
    }
    assert_eq!(nodes[&40].type_id, "node.physics_world");
}

/// An edited Physics Boxes saved before the box spawner moved: its own
/// graph (not the catalog's) with the four controls and their cards on the
/// world, a Spacing value of 3 and an LFO on Spacing. After load the value
/// drives the copies body under the same id and the LFO still resolves, so
/// the boxes come back laid out the same. The Physics Boxes rename table
/// applies to every instance of the preset before any migration, so a
/// rename there would drop the value and orphan the LFO.
#[test]
fn edited_physics_boxes_keep_box_values_and_drivers_through_load() {
    use manifold_core::effects::ParameterDriver;
    use manifold_core::layer::Layer;
    use manifold_core::preset_type_id::PresetTypeId;
    use manifold_core::project::Project;
    use manifold_core::types::{BeatDivision, DriverWaveform};

    // The old shape, rebuilt from the bundled file: values and cards on the
    // world (doc id 40), none on the box body.
    let mut old: serde_json::Value = serde_json::from_str(PHYSICS_BOXES_JSON).unwrap();
    let nodes = old["nodes"].as_array_mut().unwrap();
    let body = nodes.iter().position(|n| n["nodeId"] == "physics_demo_121").unwrap();
    let world = nodes.iter().position(|n| n["nodeId"] == "physics_demo_40").unwrap();
    for param in manifold_core::copies_body::COPY_PARAMS {
        let value = nodes[body]["params"].as_object_mut().unwrap().remove(param).unwrap();
        nodes[world]["params"][param] = value;
    }
    nodes[body].as_object_mut().unwrap().remove("exposedParams");
    nodes[world]["exposedParams"] = serde_json::json!(["copy_count", "reset"]);
    // The cards a saved show carries: the authored Count plus the stamped
    // Spacing, Columns and Arrangement, all on the world under 40_ ids.
    let meta = &mut old["presetMetadata"];
    let is_spawner = |id: &serde_json::Value| {
        id.as_str().is_some_and(|id| id.starts_with("40_copy_") && id != "40_copy_count")
    };
    meta["params"].as_array_mut().unwrap().retain(|p| !is_spawner(&p["id"]));
    meta["bindings"].as_array_mut().unwrap().retain(|b| !is_spawner(&b["id"]));
    for (param, name, default, convert) in [
        ("copy_spacing", "Spacing", 1.85, "Float"),
        ("copy_columns", "Columns", 24.0, "IntRound"),
        ("copy_layout", "Arrangement", 1.0, "EnumRound"),
    ] {
        let id = format!("40_{param}");
        meta["params"].as_array_mut().unwrap().push(serde_json::json!(
            {"id": id, "name": name, "min": 0.0, "max": 100.0, "defaultValue": default, "cardVisible": false}));
        meta["bindings"].as_array_mut().unwrap().push(serde_json::json!(
            {"id": id, "label": name, "defaultValue": default,
             "target": {"kind": "node", "nodeId": "physics_demo_40", "param": param}, "convert": {"type": convert}}));
    }
    for binding in meta["bindings"].as_array_mut().unwrap() {
        if binding["id"] == "40_copy_count" {
            binding["target"]["nodeId"] = "physics_demo_40".into();
        }
    }
    meta.as_object_mut().unwrap().remove("paramAliases");
    let old: EffectGraphDef = serde_json::from_value(old).unwrap();
    assert!(manifold_core::copies_body::carries_world_copies(&old));

    let mut layer = Layer::new_generator("Boxes".into(), PresetTypeId::from_string("PhysicsBoxes".into()), 0);
    let fx = layer.gen_params_mut().unwrap();
    fx.graph = Some(old);
    fx.refresh_manifest_from_graph();
    assert!(fx.set_base_param("40_copy_spacing", 3.0), "the old graph has a Spacing card");
    fx.drivers = Some(vec![ParameterDriver::new("40_copy_spacing", BeatDivision::Quarter, DriverWaveform::Sine)]);
    let mut project = Project::default();
    project.timeline.layers.push(layer);

    let saved = serde_json::to_string(&project).unwrap();
    let mut loaded: Project = serde_json::from_str(&saved).unwrap();
    loaded.on_after_deserialize();

    let fx = loaded.timeline.layers[0].gen_params().unwrap();
    assert!((fx.get_base_param("40_copy_spacing") - 3.0).abs() < 1e-6, "the saved Spacing survives");
    let driver = &fx.drivers.as_ref().unwrap()[0];
    assert_eq!(driver.param_id, "40_copy_spacing", "the LFO still points at Spacing");
    assert!(fx.params.iter().any(|p| p.id() == "40_copy_spacing"), "and Spacing exists");
    let def = fx.graph.as_ref().unwrap();
    let binding = def.preset_metadata.as_ref().unwrap().bindings.iter().find(|b| b.id == "40_copy_spacing").unwrap();
    assert!(
        matches!(&binding.target, BindingTarget::Node { node_id, param }
            if node_id.as_str() == "physics_demo_121" && param == "copy_spacing"),
        "Spacing drives the copies body"
    );
}
