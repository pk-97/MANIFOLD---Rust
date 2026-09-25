//! CPU qualification for the stock v3 scene-modifier recipe files.
//!
//! These tests exercise authored files through the canonical host attachment,
//! preparation, frame capture, and expansion path.  They prove graph shape and
//! routing only; the frozen cube fixture is structural evidence, while the
//! mushroom import is the production photoscan source used for frame capture.

use std::collections::BTreeMap;
use std::path::Path;

use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, InterfacePortDef, SerializedParamValue,
};
use manifold_core::scene_modifier_edit::insert_scene_modifier;
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::node_graph::scene_modifier_authoring::prepare_new_scene_modifier;
use manifold_renderer::node_graph::scene_modifier_expand::{
    SceneModifierExpandError, prepare_scene_modifiers,
};

const MUSHROOM_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/cc0___mushroom.glb"
);
const NESTED_MULTIMATERIAL_V2: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/scene-modifiers/nested_multimaterial_v2.json"
));

fn recipe(name: &str) -> EffectGraphDef {
    let file = match name {
        "SceneLoop" => include_str!("../assets/scene-modifier-presets/SceneLoop.json"),
        "SceneFog" => include_str!("../assets/scene-modifier-presets/SceneFog.json"),
        "RenderMode" => include_str!("../assets/scene-modifier-presets/RenderMode.json"),
        "ElasticSculpture" => {
            include_str!("../assets/scene-modifier-presets/ElasticSculpture.json")
        }
        "SurfacePeel" => include_str!("../assets/scene-modifier-presets/SurfacePeel.json"),
        "SurfacePeelHit" => {
            include_str!("../assets/scene-modifier-presets/SurfacePeelHit.json")
        }
        "VortexFragments" => {
            include_str!("../assets/scene-modifier-presets/VortexFragments.json")
        }
        _ => panic!("unknown stock recipe {name}"),
    };
    serde_json::from_str(file).expect("stock recipe must deserialize")
}

fn synthetic_host() -> EffectGraphDef {
    let mut host: EffectGraphDef = serde_json::from_str(NESTED_MULTIMATERIAL_V2)
        .expect("nested stock fixture must deserialize");
    host.version = 3;
    host.nodes.push(node(32, "scan_lens", "node.camera_lens"));
    host.wires
        .retain(|wire| !(wire.from_node == 31 && wire.to_node == 1 && wire.to_port == "camera"));
    host.wires.push(wire(31, "out", 32, "camera"));
    host.wires.push(wire(32, "out", 1, "camera"));
    host
}

fn render_scene_ref(host: &EffectGraphDef) -> SceneNodeRef {
    let node = host
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .expect("host must contain render_scene");
    SceneNodeRef {
        scope: Vec::new(),
        node: node.node_id.clone(),
    }
}

fn attach(
    host: EffectGraphDef,
    recipe_name: &str,
    id: &str,
    capture_frames: bool,
) -> EffectGraphDef {
    let scene = render_scene_ref(&host);
    let instance = prepare_new_scene_modifier(
        &host,
        &recipe(recipe_name),
        id.into(),
        scene,
        SceneTargetSelection::AllObjects,
    )
    .expect("fresh stock recipe must prepare against the host");
    if capture_frames {
        assert!(
            !instance.mesh_frames.is_empty(),
            "photoscan capture must be nonempty"
        );
    }
    insert_scene_modifier(&host, host.scene_modifiers.len(), instance)
        .expect("stock attachment must reconcile host metadata")
        .graph
}

fn node(id: u32, node_id: &str, type_id: &str) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: node_id.into(),
        type_id: type_id.into(),
        handle: Some(node_id.into()),
        params: BTreeMap::new(),
        exposed_params: Default::default(),
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

fn host_with_existing_instances() -> EffectGraphDef {
    let mut host = synthetic_host();
    for (group_id, input_id, object_id) in [(10, 18, 16), (20, 28, 26)] {
        {
            let group = host
                .nodes
                .iter_mut()
                .find(|node| node.id == group_id)
                .unwrap();
            let body = group.group.as_mut().unwrap();
            body.interface.inputs.push(InterfacePortDef {
                name: "instances".into(),
                port_type: "Array(InstanceTransform)".into(),
            });
            body.nodes.push(node(
                input_id,
                &format!("existing_instances_{group_id}"),
                "system.group_input",
            ));
            body.wires
                .push(wire(input_id, "instances", object_id, "instances"));
        }
        host.wires.push(wire(35, "out", group_id, "instances"));
    }
    let mut source = node(35, "existing_scene_array", "node.scene_array");
    source.params.insert(
        "pattern_length".into(),
        SerializedParamValue::Int { value: 1 },
    );
    source.params.insert(
        "cell_size".into(),
        SerializedParamValue::Float { value: 8.0 },
    );
    host.nodes.push(source);
    host
}

fn host_with_existing_atmosphere() -> EffectGraphDef {
    let mut host = synthetic_host();
    host.nodes
        .push(node(36, "existing_atmosphere", "node.atmosphere"));
    host.wires.push(wire(36, "atmosphere", 1, "atmosphere"));
    host
}

#[test]
fn all_stock_files_prepare_through_canonical_host_path() {
    let registry = PrimitiveRegistry::with_builtin();
    let (mushroom, _) = assemble_import_graph(Path::new(MUSHROOM_FIXTURE))
        .expect("production mushroom import must assemble");
    for (name, host, capture) in [
        ("ElasticSculpture", mushroom.clone(), true),
        ("SurfacePeel", mushroom.clone(), true),
        ("SurfacePeelHit", mushroom.clone(), true),
        ("VortexFragments", mushroom.clone(), true),
        ("SceneLoop", synthetic_host(), false),
        ("SceneFog", synthetic_host(), false),
        ("RenderMode", synthetic_host(), false),
    ] {
        let attached = attach(host, name, &format!("stock_{name}"), capture);
        let prepared = prepare_scene_modifiers(&attached, &registry)
            .unwrap_or_else(|error| panic!("{name} must prepare canonically: {error:?}"));
        assert!(
            !prepared.def.nodes.is_empty(),
            "{name} must expand to ordinary graph nodes"
        );
        assert!(
            prepared.def.scene_modifiers.is_empty(),
            "{name} derived graph clears authored stack"
        );
        assert!(
            attached
                .preset_metadata
                .as_ref()
                .unwrap()
                .bindings
                .iter()
                .any(|binding| binding.id.starts_with("sceneModifier:")),
            "{name} must preserve a host macro identity"
        );
    }
}

#[test]
fn loop_far_calibration_accepts_small_scenes_without_changing_camera_distance() {
    let loop_recipe = recipe("SceneLoop");
    for depth in [0.001_f32, 0.01, 0.124, 0.125, 0.126, 4.0, 2000.0] {
        let mut host = synthetic_host();
        host.preset_metadata.as_mut().unwrap().scene_bounds =
            Some(([0.0; 3], [1.0, 1.0, depth]));
        let instance = prepare_new_scene_modifier(
            &host,
            &loop_recipe,
            "small_scene_loop".into(),
            render_scene_ref(&host),
            SceneTargetSelection::AllObjects,
        )
        .unwrap_or_else(|error| panic!("Scene Loop must accept depth {depth}: {error}"));
        let metadata = instance.graph.preset_metadata.as_ref().unwrap();
        let far = metadata
            .params
            .iter()
            .find(|param| param.id == "far")
            .unwrap();
        // Preserve scene-relative framing, including below the old 1.0 minimum.
        assert_eq!(far.default_value, depth * 8.0);
        assert_eq!(far.min, 1.0_f32.min(far.default_value));
        assert!((far.min..=far.max).contains(&far.default_value));
        let far_binding = metadata
            .bindings
            .iter()
            .find(|binding| binding.id == "far")
            .unwrap();
        assert_eq!(far_binding.default_value, far.default_value);
    }
}

#[test]
fn fresh_calibration_requires_positive_extent_only_on_referenced_axes() {
    let loop_recipe = recipe("SceneLoop");
    let mut host = synthetic_host();
    let scene = render_scene_ref(&host);
    host.preset_metadata.as_mut().unwrap().scene_bounds = Some(([0.0; 3], [0.0, 0.0, 4.0]));
    assert!(
        prepare_new_scene_modifier(
            &host,
            &loop_recipe,
            "bounds_test".into(),
            scene.clone(),
            SceneTargetSelection::AllObjects
        )
        .is_ok()
    );
    host.preset_metadata.as_mut().unwrap().scene_bounds = Some(([0.0; 3], [1.0, 1.0, 0.0]));
    assert!(matches!(
        prepare_new_scene_modifier(
            &host,
            &loop_recipe,
            "bounds_test".into(),
            scene,
            SceneTargetSelection::AllObjects
        ),
        Err(SceneModifierExpandError::UnsupportedCoordinateFrame { .. })
    ));
}

#[test]
fn loop_uses_one_shared_corridor_and_camera_switch_bypass() {
    let host = attach(synthetic_host(), "SceneLoop", "loop_stock", false);
    let registry = PrimitiveRegistry::with_builtin();
    let prepared = prepare_scene_modifiers(&host, &registry).expect("loop must prepare");
    let arrays: Vec<_> = prepared
        .def
        .nodes
        .iter()
        .filter(|node| node.type_id == "node.scene_array")
        .collect();
    assert_eq!(
        arrays.len(),
        1,
        "loop must retain one shared scene_array producer"
    );
    let array_id = arrays[0].id;
    let object_inputs = prepared
        .def
        .nodes
        .iter()
        .filter(|node| node.type_id == "node.scene_object")
        .map(|node| node.id)
        .filter(|id| {
            prepared.def.wires.iter().any(|wire| {
                wire.from_node == array_id && wire.to_node == *id && wire.to_port == "instances"
            })
        })
        .count();
    assert_eq!(
        object_inputs, 2,
        "the shared corridor must feed both objects"
    );

    let lens_id = prepared
        .def
        .nodes
        .iter()
        .find(|node| node.type_id == "node.camera_lens")
        .expect("synthetic host lens must survive")
        .id;
    let switch = prepared
        .def
        .nodes
        .iter()
        .find(|node| node.type_id == "node.camera_switch")
        .expect("loop switch must be present");
    assert!(
        prepared
            .def
            .wires
            .iter()
            .any(|wire| wire.from_node == switch.id
                && wire.to_node == lens_id
                && wire.to_port == "camera")
    );
    assert!(
        prepared
            .def
            .wires
            .iter()
            .any(|wire| wire.to_node == switch.id && wire.to_port == "a")
    );

    let mut disabled = prepared.def.clone();
    let switch = disabled
        .nodes
        .iter_mut()
        .find(|node| node.type_id == "node.camera_switch")
        .unwrap();
    switch
        .params
        .insert("select".into(), SerializedParamValue::Enum { value: 0 });
    assert_eq!(
        switch.params.get("select"),
        Some(&SerializedParamValue::Enum { value: 0 }),
        "Camera Travel off selects the previous camera"
    );
    assert!(
        disabled
            .nodes
            .iter()
            .any(|node| node.type_id == "node.scene_array"),
        "disabling camera travel must retain corridor instances"
    );
}

#[test]
fn photoscan_frames_and_duplicate_elastic_controls_remain_independent() {
    let (mushroom, _) = assemble_import_graph(Path::new(MUSHROOM_FIXTURE))
        .expect("production mushroom import must assemble");
    let first = attach(mushroom, "ElasticSculpture", "elastic_a", true);
    let second = attach(first, "ElasticSculpture", "elastic_b", true);
    let frames_a = &second.scene_modifiers[0].mesh_frames;
    let frames_b = &second.scene_modifiers[1].mesh_frames;
    assert!(!frames_a.is_empty() && frames_a.len() == frames_b.len());
    assert!(frames_a.iter().all(|frame| frame.scene_radius > 0.0));

    let prepared = prepare_scene_modifiers(&second, &PrimitiveRegistry::with_builtin())
        .expect("duplicate Elastic modifiers must prepare");
    for id in ["elastic_a", "elastic_b"] {
        let route = prepared
            .routes
            .iter()
            .find(|route| {
                route.modifier_id.as_str() == id && route.local.node.as_str() == "shear_x"
            })
            .expect("each Elastic instance must retain a live shear route");
        assert_eq!(route.copies.len(), frames_a.len());
    }
    let targets: Vec<_> = second
        .preset_metadata
        .as_ref()
        .unwrap()
        .bindings
        .iter()
        .filter_map(|binding| match &binding.target {
            manifold_core::effect_graph_def::BindingTarget::SceneModifier {
                modifier_id,
                param_id,
            } if param_id == "phase" => Some(modifier_id.as_str().to_string()),
            _ => None,
        })
        .collect();
    assert!(targets.contains(&"elastic_a".into()) && targets.contains(&"elastic_b".into()));
    assert_ne!(
        second
            .preset_metadata
            .as_ref()
            .unwrap()
            .bindings
            .iter()
            .find(|b| b.id.contains("elastic_a"))
            .map(|b| &b.id),
        second
            .preset_metadata
            .as_ref()
            .unwrap()
            .bindings
            .iter()
            .find(|b| b.id.contains("elastic_b"))
            .map(|b| &b.id),
        "duplicate Elastic instances must receive independent host IDs"
    );
}

#[test]
fn source_modifiers_refuse_existing_instance_or_atmosphere_producers() {
    let registry = PrimitiveRegistry::with_builtin();
    let loop_host = attach(
        host_with_existing_instances(),
        "SceneLoop",
        "loop_conflict",
        false,
    );
    assert!(
        prepare_scene_modifiers(&loop_host, &registry).is_err(),
        "Loop must reject an already-authored instances producer"
    );
    let fog_host = attach(
        host_with_existing_atmosphere(),
        "SceneFog",
        "fog_conflict",
        false,
    );
    assert!(
        prepare_scene_modifiers(&fog_host, &registry).is_err(),
        "Fog must reject an already-authored atmosphere producer"
    );
    for name in ["SceneLoop", "SceneFog", "RenderMode"] {
        let metadata = recipe(name).preset_metadata.unwrap();
        assert!(
            metadata.scene_modifier.unwrap().singleton,
            "{name} remains a singleton source"
        );
    }
}

#[test]
fn render_mode_stage_resolves_to_render_scene_render_mode_input() {
    let registry = PrimitiveRegistry::with_builtin();
    let host = attach(synthetic_host(), "RenderMode", "rm_stock", false);
    let prepared = prepare_scene_modifiers(&host, &registry).expect("render mode must prepare");
    let producer = prepared
        .def
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_mode")
        .expect("expanded graph must carry the node.render_mode producer");
    let scene = prepared
        .def
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .expect("synthetic host scene must survive expansion");
    assert!(
        prepared.def.wires.iter().any(|wire| {
            wire.from_node == producer.id
                && wire.from_port == "render_mode"
                && wire.to_node == scene.id
                && wire.to_port == "render_mode"
        }),
        "the stage endpoint must resolve to render_scene's `render_mode` input \
         (SCENE_RENDER_MODE_DESIGN.md section 3)"
    );
    // The enable gate must be in the expanded graph: enabled × mode → Mul →
    // the atom's port-shadowed mode scalar (D5).
    let mul = prepared
        .def
        .nodes
        .iter()
        .find(|node| node.type_id == "node.math")
        .expect("gate Mul must survive expansion");
    assert!(
        prepared.def.wires.iter().any(|wire| {
            wire.from_node == mul.id && wire.to_node == producer.id && wire.to_port == "mode"
        }),
        "the enable gate must multiply into the atom's port-shadowed `mode` input"
    );
}

#[test]
fn render_mode_refuses_an_existing_render_mode_producer() {
    let registry = PrimitiveRegistry::with_builtin();
    let mut host = synthetic_host();
    host.nodes
        .push(node(36, "existing_render_mode", "node.render_mode"));
    host.wires.push(wire(36, "render_mode", 1, "render_mode"));
    let host = attach(host, "RenderMode", "rm_conflict", false);
    assert!(
        prepare_scene_modifiers(&host, &registry).is_err(),
        "Render Mode must reject an already-authored render_mode producer"
    );
}
