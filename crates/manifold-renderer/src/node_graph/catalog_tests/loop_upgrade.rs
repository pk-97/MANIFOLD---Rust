#[cfg(test)]
mod tests {
    use manifold_nodes_scene::node_graph::scene_modifier_legacy_migration::loop_upgrade::{node_mut, upgrade_known_loop};
    use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, SerializedParamValue};
    use manifold_core::effects::ParamConvert;

    fn frozen_fixture() -> EffectGraphDef {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/scene-modifiers/scene_loop_applied_v2.json"
        )))
        .expect("frozen Scene Loop fixture")
    }

    fn old_shape(jitter_period: f32, stride: f32) -> (EffectGraphDef, String, String, String) {
        let mut def = frozen_fixture();
        let array_doc = def
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "scene_array")
            .map(|node| node.id)
            .expect("scene_array");
        let camera_doc = def
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "loop_camera")
            .map(|node| node.id)
            .expect("loop_camera");
        let array = node_mut(&mut def, array_doc);
        array.params.remove("pattern_length");
        array.params.insert(
            "count".to_string(),
            SerializedParamValue::Float { value: 8.0 },
        );
        array.params.insert(
            "jitter_period".to_string(),
            SerializedParamValue::Float {
                value: jitter_period,
            },
        );
        let camera = node_mut(&mut def, camera_doc);
        camera.params.remove("patterns_per_loop");
        camera.params.remove("pattern_length");
        camera.params.insert(
            "stride".to_string(),
            SerializedParamValue::Float { value: stride },
        );
        // Force the helper to exercise the missing-wire path.
        def.wires.retain(|wire| {
            !(wire.from_node == camera_doc && wire.to_node == array_doc && wire.to_port == "camera")
        });

        let metadata = def.preset_metadata.as_mut().expect("metadata");
        // Fixed-row saves predate the declarative shared consumers.  Remove
        // the corridor-only secondary bindings so the upgrade must recreate
        // them by cloning each surviving primary slot.
        metadata.bindings.retain(|binding| {
            !matches!(
                &binding.target,
                BindingTarget::Node { node_id, param }
                    if (node_id.as_str() == "loop_camera" && param == "pattern_length")
                        || (node_id.as_str() == "scene_array" && param == "cell_size")
            )
        });
        let count_index = metadata
            .bindings
            .iter()
            .find(|binding| {
                matches!(
                    &binding.target,
                    BindingTarget::Node { node_id, param }
                        if node_id.as_str() == "scene_array" && param == "pattern_length"
                )
            })
            .map(|binding| binding.id.clone())
            .expect("pattern binding");
        let count_index = metadata
            .bindings
            .iter()
            .position(|binding| binding.id == count_index)
            .expect("pattern binding index");
        metadata.bindings[count_index].target = BindingTarget::Node {
            node_id: manifold_core::NodeId::new("scene_array"),
            param: "count".to_string(),
        };
        let count_id = metadata.bindings[count_index].id.clone();
        let mut jitter_binding = metadata.bindings[count_index].clone();
        let stride_index = metadata
            .bindings
            .iter()
            .find(|binding| {
                matches!(
                    &binding.target,
                    BindingTarget::Node { node_id, param }
                        if node_id.as_str() == "loop_camera" && param == "patterns_per_loop"
                )
            })
            .map(|binding| binding.id.clone())
            .expect("stride binding");
        let stride_index = metadata
            .bindings
            .iter()
            .position(|binding| binding.id == stride_index)
            .expect("stride binding index");
        metadata.bindings[stride_index].target = BindingTarget::Node {
            node_id: manifold_core::NodeId::new("loop_camera"),
            param: "stride".to_string(),
        };
        let stride_id = metadata.bindings[stride_index].id.clone();

        // Add a retired jitter row with its own stable id.  Its binding and
        // spec must disappear together during the upgrade.
        jitter_binding.id = "legacy_jitter".to_string();
        jitter_binding.label = "Jitter Period".to_string();
        jitter_binding.target = BindingTarget::Node {
            node_id: manifold_core::NodeId::new("scene_array"),
            param: "jitter_period".to_string(),
        };
        metadata.bindings.push(jitter_binding);
        let mut jitter_spec = metadata
            .params
            .iter()
            .find(|spec| spec.id == count_id)
            .cloned()
            .expect("count spec");
        jitter_spec.id = "legacy_jitter".to_string();
        jitter_spec.name = "Jitter Period".to_string();
        metadata.params.push(jitter_spec);
        (def, count_id, stride_id, "legacy_jitter".to_string())
    }

    #[test]
    fn known_fixture_is_current_shape_and_upgrade_is_idempotent() {
        let mut def = frozen_fixture();
        assert!(!upgrade_known_loop(&mut def).expect("current shape is valid"));
        assert_eq!(def, frozen_fixture());

        let (mut old, _, _, _) = old_shape(3.0, 7.0);
        assert!(upgrade_known_loop(&mut old).expect("old shape upgrades"));
        let upgraded = old.clone();
        assert!(!upgrade_known_loop(&mut old).expect("second run is a no-op"));
        assert_eq!(old, upgraded);
    }

    #[test]
    fn upgrade_applies_d7_rounding_preserves_handle_and_adds_camera_wire() {
        let (mut def, _, _, _) = old_shape(3.4, 7.4);
        let camera_doc = def
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "loop_camera")
            .map(|node| node.id)
            .expect("loop_camera");
        node_mut(&mut def, camera_doc).handle = Some("authored-camera-handle".to_string());
        assert!(upgrade_known_loop(&mut def).expect("old shape upgrades"));

        let array = def
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "scene_array")
            .expect("scene_array");
        let camera = def
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "loop_camera")
            .expect("loop_camera");
        assert_eq!(
            array.params.get("pattern_length"),
            Some(&SerializedParamValue::Float { value: 3.0 })
        );
        assert_eq!(
            camera.params.get("patterns_per_loop"),
            Some(&SerializedParamValue::Float { value: 2.0 })
        );
        assert_eq!(
            camera.params.get("pattern_length"),
            Some(&SerializedParamValue::Float { value: 3.0 })
        );
        assert!(!array.params.contains_key("count"));
        assert!(!camera.params.contains_key("stride"));
        assert_eq!(camera.handle.as_deref(), Some("authored-camera-handle"));
        assert!(def.wires.iter().any(|wire| {
            wire.from_node == camera.id && wire.to_node == array.id && wire.to_port == "camera"
        }));
    }

    #[test]
    fn upgrade_preserves_existing_binding_ids_and_removes_jitter_row() {
        let (mut def, count_id, stride_id, jitter_id) = old_shape(2.0, 6.0);
        assert!(upgrade_known_loop(&mut def).expect("old shape upgrades"));
        let metadata = def.preset_metadata.as_ref().expect("metadata");
        let count = metadata
            .bindings
            .iter()
            .find(|binding| binding.id == count_id)
            .expect("count binding");
        let stride = metadata
            .bindings
            .iter()
            .find(|binding| binding.id == stride_id)
            .expect("stride binding");
        assert!(
            matches!(&count.target, BindingTarget::Node { node_id, param } if node_id.as_str() == "scene_array" && param == "pattern_length")
        );
        assert!(
            matches!(&stride.target, BindingTarget::Node { node_id, param } if node_id.as_str() == "loop_camera" && param == "patterns_per_loop")
        );
        assert!(
            !metadata
                .bindings
                .iter()
                .any(|binding| binding.id == jitter_id)
        );
        assert!(!metadata.params.iter().any(|spec| spec.id == jitter_id));
        let count_spec = metadata
            .params
            .iter()
            .find(|spec| spec.id == count_id)
            .expect("count spec");
        let stride_spec = metadata
            .params
            .iter()
            .find(|spec| spec.id == stride_id)
            .expect("stride spec");
        assert_eq!(count_spec.name, "Pattern");
        assert_eq!(count_spec.default_value, 2.0);
        assert_eq!(stride_spec.name, "Stride");
        assert_eq!(stride_spec.default_value, 3.0);

        let pattern_bindings: Vec<_> = metadata
            .bindings
            .iter()
            .filter(|binding| {
                matches!(
                    &binding.target,
                    BindingTarget::Node { node_id, param }
                        if matches!(node_id.as_str(), "scene_array" | "loop_camera")
                            && param == "pattern_length"
                )
            })
            .collect();
        assert_eq!(pattern_bindings.len(), 2, "Pattern has a camera fanout");
        assert!(pattern_bindings.iter().all(|binding| {
            binding.id == count_id
                && binding.label == "Pattern"
                && binding.default_value == 2.0
                && binding.convert == ParamConvert::IntRound
        }));

        let spacing_bindings: Vec<_> = metadata
            .bindings
            .iter()
            .filter(|binding| {
                matches!(
                    &binding.target,
                    BindingTarget::Node { node_id, param }
                        if (node_id.as_str() == "loop_camera" || node_id.as_str() == "scene_array")
                            && param == "cell_size"
                )
            })
            .collect();
        assert_eq!(spacing_bindings.len(), 2, "Spacing has an array fanout");
        assert_eq!(
            spacing_bindings[0].id, spacing_bindings[1].id,
            "fanout reuses the primary identity"
        );
        assert_eq!(
            spacing_bindings[0].scale, spacing_bindings[1].scale,
            "fanout reuses affine scale"
        );
        assert_eq!(
            spacing_bindings[0].offset, spacing_bindings[1].offset,
            "fanout reuses affine offset"
        );
    }

    #[test]
    fn unsupported_legacy_numeric_storage_preserves_original() {
        let (mut def, _, _, _) = old_shape(2.0, 6.0);
        let array_doc = def
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "scene_array")
            .map(|node| node.id)
            .expect("scene_array");
        node_mut(&mut def, array_doc).params.insert(
            "jitter_period".to_string(),
            SerializedParamValue::Int { value: 2 },
        );
        let before = def.clone();
        assert!(!upgrade_known_loop(&mut def).expect("unsupported shape is skipped"));
        assert_eq!(def, before);
    }

    #[test]
    fn incomplete_or_duplicate_signature_is_byte_preserving() {
        let (mut pre_switch, _, _, _) = old_shape(2.0, 2.0);
        let switch_index = pre_switch
            .nodes
            .iter()
            .position(|node| node.node_id.as_str() == "loop_cam_switch")
            .expect("loop switch");
        pre_switch.nodes.remove(switch_index);
        let before = pre_switch.clone();
        assert!(upgrade_known_loop(&mut pre_switch).is_err());
        assert_eq!(pre_switch, before);

        let (mut partial, _, _, _) = old_shape(2.0, 2.0);
        partial
            .nodes
            .retain(|node| node.node_id.as_str() != "loop_camera");
        let before = partial.clone();
        assert!(upgrade_known_loop(&mut partial).is_err());
        assert_eq!(partial, before);

        let (mut duplicate, _, _, _) = old_shape(2.0, 2.0);
        let camera = duplicate
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "loop_camera")
            .cloned()
            .expect("loop camera");
        duplicate.nodes.push(camera);
        let before = duplicate.clone();
        assert!(upgrade_known_loop(&mut duplicate).is_err());
        assert_eq!(duplicate, before);
    }
}
