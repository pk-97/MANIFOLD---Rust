use manifold_node_engine::load::expand::{resolve_modifier_mesh_frames, validate_modifier_mesh_frames};
use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use manifold_core::scene_modifier_preset::{
    SceneContextValue, SceneModifierInstanceDef, SceneNodeRef,
    SceneStageSource, SceneTargetSelection,
};
    use manifold_core::NodeId;
    use manifold_core::effect_graph_def::BindingTarget;
    use manifold_core::scene_modifier_preset::{
        SceneModifierRecipe, SceneModifierStageDef, SceneStageInput, SceneStageScope,
    };

    fn fixture() -> (EffectGraphDef, SceneModifierInstanceDef) {
        let mut owner: EffectGraphDef = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/scene-modifiers/nested_multimaterial_v2.json"
        )))
        .unwrap();
        for group in owner
            .nodes
            .iter_mut()
            .filter_map(|node| node.group.as_mut())
        {
            let mesh = group
                .nodes
                .iter_mut()
                .find(|node| node.type_id == "node.cube_mesh")
                .unwrap();
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
                .unwrap();
            transform.params.retain(|key, _| key.starts_with("pos_"));
        }
        let mut graph = owner.clone();
        graph.version = 3;
        graph.nodes.clear();
        graph.wires.clear();
        graph.preset_metadata.as_mut().unwrap().scene_modifier = Some(SceneModifierRecipe {
            shatter: None,
            schema_version: 1,
            singleton: false,
            enabled_param: "enabled".into(),
            preparation_params: vec![],
            impulses: vec![],
            initializers: vec![],
            calibrations: vec![],
            stages: vec![SceneModifierStageDef {
                group: NodeId::new("deform"),
                scope: SceneStageScope::EachObject,
                inputs: vec![SceneStageInput {
                    port: "radius".into(),
                    source: SceneStageSource::Context {
                        value: SceneContextValue::SceneRadius,
                    },
                }],
                outputs: vec![],
            }],
        });
        let instance = SceneModifierInstanceDef {
            id: NodeId::new("modifier"),
            scene: SceneNodeRef {
                scope: vec![],
                node: NodeId::new("scan_render"),
            },
            targets: SceneTargetSelection::AllObjects,
            mesh_frames: vec![],
            legacy_math_view_carrier: None,
            graph: Box::new(graph),
        };
        (owner, instance)
    }

    #[test]
    fn scene_modifier_coordinate_context_preserves_saved_frame_on_motion_rebuild_and_reopen() {
        let (mut owner, mut instance) = fixture();
        instance.mesh_frames = resolve_modifier_mesh_frames(&owner, &instance).unwrap();
        assert_eq!(instance.mesh_frames.len(), 2);
        assert_eq!(instance.mesh_frames[0].scene_radius, 41.0_f64.sqrt() / 2.0);
        assert_eq!(instance.mesh_frames[0].source_offset[0], -0.75);
        let saved = instance.mesh_frames.clone();
        let group = owner
            .nodes
            .iter_mut()
            .find_map(|node| node.group.as_mut())
            .unwrap();
        let transform = group
            .nodes
            .iter_mut()
            .find(|node| node.type_id == "node.transform_3d")
            .unwrap();
        transform
            .params
            .insert("pos_x".into(), SerializedParamValue::Float { value: 50.0 });
        transform
            .params
            .insert("rot_y".into(), SerializedParamValue::Float { value: 1.0 });
        owner.preset_metadata.as_mut().unwrap().scene_bounds = None;
        let instance: SceneModifierInstanceDef =
            serde_json::from_str(&serde_json::to_string(&instance).unwrap()).unwrap();
        validate_modifier_mesh_frames(&owner, &instance).unwrap();
        assert_eq!(
            resolve_modifier_mesh_frames(&owner, &instance).unwrap(),
            saved
        );
    }

    #[test]
    fn scene_modifier_coordinate_context_retarget_keeps_radius_and_survivors() {
        let (mut owner, mut instance) = fixture();
        let all = resolve_modifier_mesh_frames(&owner, &instance).unwrap();
        instance.targets = SceneTargetSelection::Explicit {
            objects: vec![all[0].target.clone()],
        };
        instance.mesh_frames = resolve_modifier_mesh_frames(&owner, &instance).unwrap();
        owner.preset_metadata.as_mut().unwrap().scene_bounds = Some(([-100.0; 3], [100.0; 3]));
        instance.targets = SceneTargetSelection::AllObjects;
        assert!(validate_modifier_mesh_frames(&owner, &instance).is_err());
        let result = resolve_modifier_mesh_frames(&owner, &instance).unwrap();
        assert_eq!(result, all);
        instance.mesh_frames = result;
        instance.targets = SceneTargetSelection::Explicit {
            objects: vec![all[1].target.clone()],
        };
        assert_eq!(
            resolve_modifier_mesh_frames(&owner, &instance).unwrap(),
            vec![all[1].clone()]
        );
    }

    #[test]
    fn scene_modifier_coordinate_context_rejects_source_change_without_mutation() {
        let (mut owner, mut instance) = fixture();
        instance.mesh_frames = resolve_modifier_mesh_frames(&owner, &instance).unwrap();
        let snapshot = instance.clone();
        let mesh = owner
            .nodes
            .iter_mut()
            .find_map(|node| node.group.as_mut())
            .unwrap()
            .nodes
            .iter_mut()
            .find(|node| node.type_id == "node.gltf_mesh_source")
            .unwrap();
        mesh.params.insert(
            "translate_x".into(),
            SerializedParamValue::Float { value: 1.0 },
        );
        assert!(
            resolve_modifier_mesh_frames(&owner, &instance)
                .unwrap_err()
                .to_string()
                .contains("source changed")
        );
        assert_eq!(instance, snapshot);
    }

    #[test]
    fn scene_modifier_coordinate_context_rejects_invalid_bounds_and_fresh_rotation() {
        let (mut owner, instance) = fixture();
        owner.preset_metadata.as_mut().unwrap().scene_bounds = Some(([1.0; 3], [-1.0; 3]));
        assert!(resolve_modifier_mesh_frames(&owner, &instance).is_err());
        owner.preset_metadata.as_mut().unwrap().scene_bounds = None;
        assert_eq!(
            resolve_modifier_mesh_frames(&owner, &instance).unwrap()[0].scene_radius,
            3.0
        );
        let transform = owner
            .nodes
            .iter_mut()
            .find_map(|node| node.group.as_mut())
            .unwrap()
            .nodes
            .iter_mut()
            .find(|node| node.type_id == "node.transform_3d")
            .unwrap();
        transform
            .params
            .insert("rot_y".into(), SerializedParamValue::Float { value: 0.1 });
        assert!(resolve_modifier_mesh_frames(&owner, &instance).is_err());
    }

    #[test]
    fn scene_modifier_coordinate_context_fingerprint_ignores_labels_but_tracks_bound_asset() {
        let (mut owner, mut instance) = fixture();
        instance.mesh_frames = resolve_modifier_mesh_frames(&owner, &instance).unwrap();
        owner.nodes[3].handle = Some("Renamed group".into());
        validate_modifier_mesh_frames(&owner, &instance).unwrap();
        owner
            .preset_metadata
            .as_mut()
            .unwrap()
            .string_bindings
            .push(manifold_core::effect_graph_def::StringBindingDef {
                id: "model_file".into(),
                label: "File".into(),
                default_value: "different.glb".into(),
                target: BindingTarget::Node {
                    node_id: NodeId::new("left_mesh"),
                    param: "path".into(),
                },
            });
        assert!(validate_modifier_mesh_frames(&owner, &instance).is_err());
    }
