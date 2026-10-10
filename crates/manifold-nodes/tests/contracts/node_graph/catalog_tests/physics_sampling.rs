#[cfg(feature = "gpu-proofs")]
use manifold_node_engine::exec::effect_node::FrameTime;
use manifold_node_engine::runtime::*;
#[cfg(feature = "gpu-proofs")]
use manifold_core::{Beats, Seconds};
    use manifold_node_engine::persistence::PrimitiveRegistry;

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn scene_physics_role_history_samples_live_controls_without_rendering() {
        let mut def: serde_json::Value = serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cpu-flip/WaterBasin.json")))
        .unwrap();
        def["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": 500, "nodeId": "pouring_mesh", "typeId": "node.fluid_role_source"
            }));
        def["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": 501, "nodeId": "visible_source", "typeId": "node.cube_mesh"
            }));
        def["wires"].as_array_mut().unwrap().extend([
            serde_json::json!({"fromNode": 5, "fromPort": "transform", "toNode": 500, "toPort": "transform"}),
            serde_json::json!({"fromNode": 501, "fromPort": "source", "toNode": 500, "toPort": "mesh_0"}),
            serde_json::json!({"fromNode": 500, "fromPort": "role", "toNode": 4, "toPort": "role_0"}),
        ]);
        let runtime =
            PresetRuntime::from_json_str(&def.to_string(), &PrimitiveRegistry::with_cpu_flip_reference())
                .expect("typed fluid role ancestry loads");
        let mut saw_source = false;
        let mut saw_motion = false;
        for (step, sampled) in runtime
            .plan
            .steps()
            .iter()
            .zip(manifold_node_engine::runtime::testkit::sampling_mask(&runtime).unwrap())
        {
            let kind = runtime.graph.get_node(step.node).unwrap().node.type_id();
            match kind.as_str() {
                "node.fluid_role_source" => {
                    assert!(sampled);
                    saw_source = true;
                }
                "node.lfo" => {
                    assert!(sampled);
                    saw_motion = true;
                }
                "node.scene_object" | "node.render_scene" | "node.cube_mesh" => assert!(!sampled),
                _ => {}
            }
        }
        assert!(saw_source && saw_motion);
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn water_history_samples_fluid_controls_without_rendering() {
        let runtime = PresetRuntime::from_json_str(
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cpu-flip/WaterBasin.json")),
            &PrimitiveRegistry::with_cpu_flip_reference(),
        )
        .expect("WaterBasin loads");
        let mask = manifold_node_engine::runtime::testkit::sampling_mask(&runtime)
            .expect("fluid ancestry");
        let sampled: Vec<_> = runtime
            .plan
            .steps()
            .iter()
            .zip(mask)
            .filter(|(_, enabled)| **enabled)
            .map(|(step, _)| {
                runtime
                    .graph
                    .get_node(step.node)
                    .unwrap()
                    .node
                    .type_id()
                    .as_str()
                    .to_owned()
            })
            .collect();
        assert!(sampled.iter().any(|kind| kind == FLIP_DOMAIN_TYPE_ID));
        assert!(sampled.iter().any(|kind| kind == "node.lfo"));
        assert!(!sampled.iter().any(|kind| kind == "node.scene_object"));
        assert!(!sampled.iter().any(|kind| kind == "node.render_scene"));
    }

    #[test]
    fn matter_liquid_samples_its_field_and_its_world_per_tick() {
        let runtime = PresetRuntime::from_json_str(
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/WaterFloatingBoxMatter.json")),
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("WaterFloatingBoxMatter loads");
        let pairs = runtime.plan.coupled_scenes();
        assert!(!pairs.is_empty(), "the box and the liquid are one coupled scene");
        let mask = manifold_node_engine::runtime::testkit::sampling_mask(&runtime).expect("the liquid samples its field per tick");
        for pair in pairs {
            assert!(mask[pair.fluid_step_for_test()], "the liquid samples");
            assert!(mask[pair.rigid_step_for_test()], "its owned world's scene samples per tick");
        }
    }

    #[test]
    fn physics_history_mask_includes_nonlinear_lfo_and_excludes_rendering() {
        let mut def: serde_json::Value = serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/PhysicsSolids.json")))
        .unwrap();
        def["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": 500,
                "nodeId": "animated_x",
                "typeId": "node.lfo",
                "params": {
                    "rate_mode": { "type": "Enum", "value": 1 },
                    "angular_rate": { "type": "Float", "value": 12.0 },
                    "min": { "type": "Float", "value": -2.0 },
                    "max": { "type": "Float", "value": 2.0 }
                }
            }));
        def["wires"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "fromNode": 500, "fromPort": "out", "toNode": 100, "toPort": "pos_x"
            }));
        let runtime = PresetRuntime::from_json_str(
            &serde_json::to_string(&def).unwrap(),
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("PhysicsSolids with an LFO-authored body loads");
        let mask = manifold_node_engine::runtime::testkit::sampling_mask(&runtime)
            .expect("physics ancestry");
        let sampled: Vec<_> = runtime
            .plan
            .steps()
            .iter()
            .zip(mask)
            .filter(|(_, enabled)| **enabled)
            .map(|(step, _)| {
                runtime
                    .graph
                    .get_node(step.node)
                    .unwrap()
                    .node
                    .type_id()
                    .as_str()
                    .to_owned()
            })
            .collect();
        assert!(sampled.iter().any(|kind| kind == "node.lfo"));
        assert!(sampled.iter().any(|kind| kind == "node.physics_world"));
        assert!(!sampled.iter().any(|kind| kind == "node.render_scene"));
    }

    #[test]
    fn rigid_body_source_is_setup_and_excludes_gpu_mesh_ancestors_from_history() {
        let mut def: serde_json::Value = serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/PhysicsSolids.json")))
        .unwrap();
        // Source-driven bodies take shape from the visible mesh. Remove the
        // legacy body's opposite shape route before wiring that source back.
        def["wires"]
            .as_array_mut()
            .unwrap()
            .retain(|wire| !(wire["fromNode"] == 101 && wire["toNode"] == 102));
        def["wires"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "fromNode": 102,
                "fromPort": "source",
                "toNode": 101,
                "toPort": "source"
            }));
        let runtime = PresetRuntime::from_json_str(
            &serde_json::to_string(&def).unwrap(),
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("rigid body source graph loads");
        let mask = manifold_node_engine::runtime::testkit::sampling_mask(&runtime)
            .expect("physics ancestry");
        let sampled: Vec<_> = runtime
            .plan
            .steps()
            .iter()
            .zip(mask)
            .filter(|(_, enabled)| **enabled)
            .map(|(step, _)| {
                runtime
                    .graph
                    .get_node(step.node)
                    .unwrap()
                    .node
                    .type_id()
                    .as_str()
                    .to_owned()
            })
            .collect();
        assert!(sampled.iter().any(|kind| kind == "node.rigid_body"));
        assert!(
            !sampled
                .iter()
                .any(|kind| kind == "node.platonic_solid_mesh")
        );
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn explicit_reset_clears_physics_history_clock() {
        let mut runtime = PresetRuntime::from_json_str(
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/PhysicsSolids.json")),
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("PhysicsSolids loads");
        manifold_node_engine::runtime::testkit::set_last_physics_frame_time(&mut runtime, Some(FrameTime {
            beats: Beats(1.0),
            seconds: Seconds(1.0),
            delta: Seconds(1.0),
            frame_count: 1,
        }));
        runtime.reset_state(&manifold_gpu::testkit::test_device());
        assert!(manifold_node_engine::runtime::testkit::last_physics_frame_time(&runtime).is_none());
    }

    #[test]
    fn physics_history_holds_release_events_without_replaying_trigger_state() {
        let mut def: serde_json::Value = serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/PhysicsSolids.json")))
        .unwrap();
        def["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": 500, "nodeId": "release_event", "typeId": "node.trigger_gate"
            }));
        def["wires"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "fromNode": 500, "fromPort": "out", "toNode": 111, "toPort": "release_count"
            }));
        let registry = PrimitiveRegistry::with_builtin();
        let runtime = PresetRuntime::from_json_str(&def.to_string(), &registry)
            .expect("release events are not replayed during historical pose sampling");
        let event = runtime
            .graph
            .instance_by_node_id(&manifold_core::NodeId::new("release_event"))
            .unwrap();
        for (step, sampled) in runtime
            .plan
            .steps()
            .iter()
            .zip(manifold_node_engine::runtime::testkit::sampling_mask(&runtime).unwrap())
        {
            if step.node == event {
                assert!(!sampled);
            }
        }
        def["wires"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "fromNode": 500, "fromPort": "out", "toNode": 110, "toPort": "pos_x"
            }));
        assert!(
            PresetRuntime::from_json_str(&def.to_string(), &registry).is_err(),
            "stateful pose ancestry must still be rejected"
        );
    }

    /// Ocean Cliff's surges come from its paddle: the LFO, the paddle's
    /// transform and its collider role are replayed at every tick's start, so
    /// the waves are the same at any frame rate and never need a Reset. The
    /// sea, the rock and the renderer stay out of the per-tick passes.
    #[test]
    fn ocean_cliff_paddle_is_replayed_per_tick() {
        let runtime = PresetRuntime::from_json_str(
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/OceanCliff.json")),
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("OceanCliff loads");
        let mask = manifold_node_engine::runtime::testkit::sampling_mask(&runtime).expect("the paddle is physics ancestry");
        let sampled = |node_id: &str| {
            let node = runtime
                .graph
                .instance_by_node_id(&manifold_core::NodeId::new(node_id))
                .unwrap_or_else(|| panic!("{node_id} exists"));
            let (_, on) = runtime
                .plan
                .steps()
                .iter()
                .zip(mask)
                .find(|(step, _)| step.node == node)
                .unwrap_or_else(|| panic!("{node_id} has a step"));
            *on
        };
        for node_id in ["paddle_drive", "paddle_transform", "paddle", "domain"] {
            assert!(sampled(node_id), "{node_id} is replayed per tick");
        }
        for node_id in ["scene", "cliff_object", "ocean_object", "swell_spectrum", "sky"] {
            assert!(!sampled(node_id), "{node_id} stays out of the per-tick passes");
        }
    }

#[cfg(feature = "gpu-proofs")]
use manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID;
