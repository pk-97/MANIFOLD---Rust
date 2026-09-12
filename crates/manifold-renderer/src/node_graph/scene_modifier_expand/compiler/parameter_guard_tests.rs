use super::*;
use crate::node_graph::scene_modifier_expand::resolve_modifier_mesh_frames;

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphWire, InterfacePortDef, SerializedParamValue, StringBindingDef,
    StringParamSpecDef,
};
use manifold_core::scene_modifier_preset::{SceneContextValue, SceneStageInput, SceneStageSource};
use manifold_core::{Beats, NodeId, Seconds};

fn calibrated_fixture() -> EffectGraphDef {
    let mut owner = super::tests::fixture();

    // Replace the deterministic cube leaves with source primitives while
    // retaining the authored object placements used by the calibration.
    for group in owner
        .nodes
        .iter_mut()
        .filter_map(|node| node.group.as_mut())
    {
        let mesh = group
            .nodes
            .iter_mut()
            .find(|node| node.type_id == "node.cube_mesh")
            .expect("fixture cube source");
        mesh.type_id = "node.gltf_mesh_source".into();
        mesh.params.insert(
            "path".into(),
            SerializedParamValue::String {
                value: "scan.glb".into(),
            },
        );
        mesh.params.insert(
            "source_bbox_radius".into(),
            SerializedParamValue::Float { value: 3.0 },
        );
        let transform = group
            .nodes
            .iter_mut()
            .find(|node| node.type_id == "node.transform_3d")
            .expect("fixture transform");
        transform.params.retain(|key, _| key.starts_with("pos_"));
    }

    // The outer card's string binding is part of the source identity. Add it
    // before capturing frames so the saved hash includes its effective path.
    let metadata = owner.preset_metadata.as_mut().expect("fixture metadata");
    metadata.string_params.push(StringParamSpecDef {
        id: "mesh_path".into(),
        name: "Mesh Path".into(),
        default_value: "scan.glb".into(),
        is_file_picker: true,
        use_dropdown: false,
        is_file_path: true,
    });
    metadata.string_bindings.push(StringBindingDef {
        id: "mesh_path".into(),
        label: "Mesh Path".into(),
        default_value: "scan.glb".into(),
        target: BindingTarget::Node {
            node_id: NodeId::new("left_mesh"),
            param: "path".into(),
        },
    });

    // Make the stage consume the calibrated radius through a real scalar
    // group input. This exercises frame capture and the ordinary live
    // geometry path without needing a file or GPU.
    let modifier = owner.scene_modifiers.first_mut().expect("fixture modifier");
    let recipe = modifier
        .graph
        .preset_metadata
        .as_mut()
        .and_then(|metadata| metadata.scene_modifier.as_mut())
        .expect("fixture recipe");
    recipe.stages[0].inputs.push(SceneStageInput {
        port: "radius".into(),
        source: SceneStageSource::Context {
            value: SceneContextValue::SceneRadius,
        },
    });
    let group = modifier
        .graph
        .nodes
        .first_mut()
        .and_then(|node| node.group.as_mut())
        .expect("fixture stage group");
    group.interface.inputs.push(InterfacePortDef {
        name: "radius".into(),
        port_type: "Scalar(F32)".into(),
    });
    let mut radius_input = group
        .nodes
        .iter()
        .find(|node| node.node_id.as_str() == "group_current")
        .expect("fixture group input")
        .clone();
    radius_input.id = 6;
    radius_input.node_id = NodeId::new("group_radius");
    radius_input.handle = Some("radius".into());
    group.nodes.push(radius_input);
    group.wires.push(EffectGraphWire {
        from_node: 6,
        from_port: "radius".into(),
        to_node: 3,
        to_port: "scale".into(),
    });

    let snapshot = owner.scene_modifiers[0].clone();
    owner.scene_modifiers[0].mesh_frames =
        resolve_modifier_mesh_frames(&owner, &snapshot).expect("capture source frames");
    owner
}

fn runtime(owner: EffectGraphDef, fused: bool) -> crate::preset_runtime::PresetRuntime {
    let registry = PrimitiveRegistry::with_builtin();
    crate::preset_runtime::PresetRuntime::from_def_for_render(owner, &registry, None, fused)
        .expect("prepared scene modifier runtime")
}

fn source_id(runtime: &crate::preset_runtime::PresetRuntime) -> crate::node_graph::NodeInstanceId {
    runtime
        .graph
        .instance_by_node_id(&NodeId::new("left_mesh"))
        .expect("flattened left source")
}

fn frame_time() -> crate::node_graph::effect_node::FrameTime {
    crate::node_graph::effect_node::FrameTime {
        beats: Beats(0.0),
        seconds: Seconds(0.0),
        delta: Seconds(1.0 / 60.0),
        frame_count: 0,
    }
}

#[test]
fn scene_modifier_parameter_guard_maps_string_binding_and_suspends_invalid_frames() {
    for fused in [false, true] {
        let owner = calibrated_fixture();
        let mut runtime = runtime(owner, fused);
        let source = source_id(&runtime);

        let mut same = BTreeMap::new();
        same.insert("mesh_path".into(), "scan.glb".into());
        runtime.set_string_params(Some(&same));
        assert!(runtime.graph.prepared_param_violation().is_none());
        let source_epoch = runtime.graph.get_node(source).unwrap().param_epoch;

        let mut changed = BTreeMap::new();
        changed.insert("mesh_path".into(), "other.glb".into());
        runtime.set_string_params(Some(&changed));
        assert_eq!(
            runtime.graph.prepared_param_violation().unwrap().0.as_str(),
            "left_mesh"
        );
        assert_eq!(runtime.graph.prepared_param_violation().unwrap().1, "path");
        assert_eq!(
            runtime.graph.get_node(source).unwrap().param_epoch,
            source_epoch
        );

        runtime.execute_frame(frame_time());
        assert!(runtime.errors().iter().any(|error| matches!(
            error,
            crate::preset_runtime::ChainError::PreparedParameterChanged { node_id, param }
                if node_id == "left_mesh" && param == "path"
        )));

        runtime.set_string_params(Some(&same));
        assert!(runtime.graph.prepared_param_violation().is_none());
        // Valid GLTF execution needs prepared GPU buffers; guard recovery
        // itself is observable without evaluating a source in this CPU test.
    }
}

#[test]
fn scene_modifier_parameter_guard_blocks_source_selector_and_rt_but_keeps_geometry_live() {
    let owner = calibrated_fixture();
    let mut runtime = runtime(owner, false);
    let source = source_id(&runtime);

    let before = runtime.graph.get_node(source).unwrap().param_epoch;
    assert!(
        runtime
            .graph
            .set_param(
                source,
                "mesh_index",
                crate::node_graph::ParamValue::Float(0.0)
            )
            .is_err()
    );
    assert_eq!(runtime.graph.get_node(source).unwrap().param_epoch, before);
    runtime.graph.set_param_unchecked(
        source,
        "mesh_index",
        crate::node_graph::ParamValue::Float(0.0),
    );
    assert_eq!(runtime.graph.get_node(source).unwrap().param_epoch, before);
    assert_eq!(
        runtime.graph.prepared_param_violation().unwrap().1,
        "mesh_index"
    );

    let scene = runtime
        .graph
        .instance_by_node_id(&NodeId::new("scan_render"))
        .expect("render scene");
    assert!(
        runtime
            .graph
            .set_param(
                scene,
                "rt_enabled",
                crate::node_graph::ParamValue::Bool(true)
            )
            .is_err()
    );

    let local = manifold_core::scene_modifier_preset::SceneNodeRef {
        scope: vec![NodeId::new("elastic_stage")],
        node: NodeId::new("shear_x"),
    };
    let copy = runtime
        .modifier_node_copies(&NodeId::new("test_modifier"), &local)
        .and_then(|copies| copies.first())
        .expect("ordinary geometry copy");
    let copy_id = runtime
        .graph
        .instance_by_node_id(&copy.node_id)
        .expect("copy node");
    assert!(
        runtime
            .graph
            .set_param(
                copy_id,
                "amplitude",
                crate::node_graph::ParamValue::Float(0.31)
            )
            .is_ok()
    );
}

#[test]
fn scene_modifier_parameter_guard_rejects_initial_selector_change_before_gpu() {
    let mut owner = calibrated_fixture();
    // The authored source remains calibrated at mesh_index=-1. A live
    // manifest/default binding writes a different selector while the runtime
    // is being built; guard installation must reject that before execution.
    let metadata = owner.preset_metadata.as_mut().expect("fixture metadata");
    let recipe_metadata = owner.scene_modifiers[0]
        .graph
        .preset_metadata
        .as_ref()
        .unwrap();
    let mut spec = recipe_metadata.params[0].clone();
    spec.id = "mesh_index".into();
    spec.name = "Mesh Index".into();
    spec.min = -1.0;
    spec.max = 64.0;
    spec.default_value = 0.0;
    spec.whole_numbers = true;
    let mut binding = recipe_metadata.bindings[0].clone();
    binding.id = spec.id.clone();
    binding.label = spec.name.clone();
    binding.default_value = 0.0;
    binding.target = BindingTarget::Node {
        node_id: NodeId::new("left_mesh"),
        param: "mesh_index".into(),
    };
    metadata.params.push(spec);
    metadata.bindings.push(binding);
    let registry = PrimitiveRegistry::with_builtin();
    let result =
        crate::preset_runtime::PresetRuntime::from_def_for_render(owner, &registry, None, false);
    assert!(
        matches!(
            &result,
            Err(
                crate::preset_runtime::JsonGeneratorLoadError::SceneModifier(
                    SceneModifierExpandError::UnsupportedCoordinateFrame { .. }
                )
            )
        ),
        "unexpected result: {:?}",
        result.err()
    );
}

#[test]
fn scene_modifier_parameter_guard_accepts_canonical_asset_relocation() {
    let owner = calibrated_fixture();
    let relocated = manifold_core::scene_source_identity::relocate_scene_source_asset(
        &owner,
        "scan.glb",
        "relocated.glb",
    )
    .expect("relocation transaction")
    .expect("calibrated source relocation");
    let mut runtime = runtime(relocated, false);
    let mut path = BTreeMap::new();
    path.insert("mesh_path".into(), "relocated.glb".into());
    runtime.set_string_params(Some(&path));
    assert!(runtime.graph.prepared_param_violation().is_none());
}
