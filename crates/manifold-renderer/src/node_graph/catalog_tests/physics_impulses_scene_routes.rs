use super::*;
use manifold_core::effect_graph_def::BindingTarget;
use manifold_core::params::{Param, ParamManifest};
use manifold_core::scene_modifier_preset::SceneModifierInstanceDef;

fn scene_fixture(targets: &[&str]) -> (EffectGraphDef, ParamManifest) {
    let mut def = fixture();
    let recipe: EffectGraphDef = serde_json::from_str(manifold_renderer::testkit::assets::ASSETS_SCENE_MODIFIER_PRESETS_UNIFORMFORCE_JSON)
    .unwrap();
    let mut metadata = recipe.preset_metadata.clone().unwrap();
    metadata.scene_modifier = None;
    metadata.params.clear();
    metadata.bindings.clear();
    def.preset_metadata = Some(metadata);
    // No continuous acceleration; isolate velocity changes from Fire.
    def.wires.retain(|wire| wire.from_node != 1);
    def = manifold_core::scene_modifier_edit::insert_scene_modifier(
        &def,
        0,
        SceneModifierInstanceDef {
            id: NodeId::new("force"),
            scene: reference("scene"),
            targets: SceneTargetSelection::Explicit {
                objects: targets.iter().map(|id| reference(id)).collect(),
            },
            mesh_frames: vec![],
            legacy_math_view_carrier: None,
            graph: Box::new(recipe),
        },
    )
    .unwrap()
    .graph;
    let mut manifest = ParamManifest::from_params(
        def.preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .cloned()
            .map(Param::bundled)
            .collect(),
    );
    for (local, value) in [
        ("strength", 0.0),
        ("direction_x", 1.0),
        ("direction_y", 0.0),
    ] {
        manifest.get_mut(&alias(&def, local)).unwrap().value = value;
    }
    (def, manifest)
}

fn alias(def: &EffectGraphDef, local: &str) -> String {
    def.preset_metadata.as_ref().unwrap().bindings.iter().find(|binding|
        matches!(&binding.target, BindingTarget::SceneModifier { param_id, .. } if param_id == local)
    ).unwrap().id.clone()
}

#[test]
fn scene_impulse_routes_capture_each_click_with_selected_body_and_edited_strength() {
    for fused in [false, true] {
        let (def, mut manifest) = scene_fixture(&["part_a", "part_a_2"]);
        let mut runtime =
            PresetRuntime::from_def_for_render(def.clone(), &registry(), Some(&manifest), fused)
                .unwrap();
        let fire = alias(&def, "fire");
        let mut sequence = 41;
        assert!(
            runtime
                .fire_scene_impulse(&fire, time(0.0), &mut sequence)
                .is_err()
        );
        runtime.execute_frame(time(0.0));
        assert!(
            runtime
                .fire_scene_impulse(&fire, time(0.0), &mut sequence)
                .unwrap()
        );
        manifest
            .get_mut(&alias(&def, "impulse_strength"))
            .unwrap()
            .value = 7.0;
        runtime.apply_param_values(&manifest);
        runtime
            .fire_scene_impulse(&fire, time(0.0), &mut sequence)
            .unwrap();
        manifest
            .get_mut(&alias(&def, "impulse_strength"))
            .unwrap()
            .value = -10.0;
        runtime.apply_param_values(&manifest);
        runtime.execute_frame(time(DT));
        let positions = POSITIONS.get();
        assert!(
            (positions[0] - 9.0 * DT as f32).abs() < 1e-5,
            "{positions:?}"
        );
        assert_eq!(positions[1], 5.0);
        let mut receipts = Vec::new();
        runtime.drain_scene_impulses(|_, event| receipts.push(event));
        assert_eq!(receipts.len(), 2);
        for (event, (sequence, strength)) in receipts.iter().zip([(41, 2.0), (42, 7.0)]) {
            assert_eq!(event.source.sequence, sequence);
            assert_eq!(event.value.field.sample([0.0; 3]), [strength, 0.0, 0.0]);
        }
        assert!(
            !runtime
                .fire_scene_impulse("ordinary-trigger", time(DT), &mut sequence)
                .unwrap()
        );
    }
}

#[test]
fn scene_impulse_routes_reload_does_not_replay_saved_counter_and_empty_targets_stay_editable() {
    for targets in [vec!["part_a"], vec![]] {
        let (def, mut manifest) = scene_fixture(&targets);
        let fire = alias(&def, "fire");
        manifest.get_mut(&fire).unwrap().value = 23.0;
        let restored: EffectGraphDef =
            serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
        let mut runtime =
            PresetRuntime::from_def_for_render(restored, &registry(), Some(&manifest), false)
                .unwrap();
        runtime.execute_frame(time(0.0));
        runtime.execute_frame(time(DT));
        runtime.drain_scene_impulses(|_, _| panic!("saved counters must not generate events"));
        let result = runtime.fire_scene_impulse(&fire, time(DT), &mut 0);
        if targets.is_empty() {
            assert!(result.unwrap_err().contains("select a simulated object"));
        } else {
            assert!(result.unwrap());
        }
    }
}

#[test]
fn scene_impulse_routes_acknowledged_receipts_allow_more_than_queue_capacity() {
    let (def, manifest) = scene_fixture(&["part_a"]);
    let mut runtime =
        PresetRuntime::from_def_for_render(def.clone(), &registry(), Some(&manifest), false)
            .unwrap();
    let fire = alias(&def, "fire");
    runtime.execute_frame(time(0.0));
    let mut sequence = 0;
    let mut receipts = 0;
    for frame in 0..300 {
        runtime
            .fire_scene_impulse(&fire, time(frame as f64 * DT), &mut sequence)
            .unwrap();
        runtime.execute_frame(time((frame + 1) as f64 * DT));
        runtime.drain_scene_impulses(|_, _| receipts += 1);
    }
    assert_eq!(receipts, 300);
}

#[test]
fn scene_impulse_routes_reset_rearms_internal_bindings_and_cancels_pending_hits() {
    let (def, manifest) = scene_fixture(&["part_a"]);
    let mut runtime =
        PresetRuntime::from_def_for_render(def.clone(), &registry(), Some(&manifest), false)
            .unwrap();
    let fire = alias(&def, "fire");
    runtime.execute_frame(time(0.0));
    runtime
        .fire_scene_impulse(&fire, time(0.0), &mut 0)
        .unwrap();
    // CPU equivalent of reset_state's identity and native reset; no GPU device needed.
    manifold_node_engine::runtime::testkit::reset_impulse_routes(&mut runtime);
    for node in runtime.graph.nodes_mut() {
        node.node.clear_state();
    }
    assert!(
        runtime
            .fire_scene_impulse(&fire, time(0.0), &mut 1)
            .is_err()
    );
    runtime.execute_frame(time(0.0));
    runtime
        .fire_scene_impulse(&fire, time(0.0), &mut 1)
        .unwrap();
    runtime.execute_frame(time(DT));
    let mut receipts = 0;
    runtime.drain_scene_impulses(|_, event| {
        receipts += 1;
        assert_eq!(event.source.sequence, 1);
    });
    assert_eq!(receipts, 1);
}

#[cfg(feature = "gpu-proofs")]
#[test]
fn scene_impulse_routes_share_rigid_and_fluid_targets_and_wait_for_domain_edits() {
    let (mut def, manifest) = scene_fixture(&["part_a", "part_b"]);
    def.nodes.extend([
        serde_json::from_value(serde_json::json!({"id":14,"nodeId":"fluid","typeId":manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID,
            "params":{"resolution":{"type":"Int","value":8},"fill_height":{"type":"Float","value":0.0},"emission":{"type":"Float","value":0.0}}})).unwrap(),
        serde_json::from_value(serde_json::json!({"id":15,"nodeId":"domain","typeId":"node.transform_3d"})).unwrap(),
    ]);
    def.wires
        .retain(|wire| !(wire.to_node == 9 && wire.to_port == "transform"));
    def.wires.extend([
        serde_json::from_value(
            serde_json::json!({"fromNode":14,"fromPort":"vertices","toNode":9,"toPort":"vertices"}),
        )
        .unwrap(),
        serde_json::from_value(
            serde_json::json!({"fromNode":15,"fromPort":"transform","toNode":14,"toPort":"domain"}),
        )
        .unwrap(),
    ]);
    let fire = alias(&def, "fire");
    let mut runtime =
        PresetRuntime::from_def_for_render(def, &registry(), Some(&manifest), false).unwrap();
    runtime.execute_frame(time(0.0));
    let mut sequence = 0;
    runtime
        .fire_scene_impulse(&fire, time(0.0), &mut sequence)
        .unwrap();
    runtime.execute_frame(time(DT));
    let mut recipients = Vec::new();
    runtime.drain_scene_impulses(|id, event| {
        assert_eq!(event.value.field.sample([0.0; 3]), [2.0, 0.0, 0.0]);
        assert!(matches!(event.value.target, ImpulseTarget::FluidAndRigid(_)));
        recipients.push(id.to_string());
    });
    recipients.sort();
    assert_eq!(recipients, ["fluid"], "one shared owner admits the source hit once");
    edit(&mut runtime, "domain", "pos_x", 2.0);
    assert!(
        runtime
            .fire_scene_impulse(&fire, time(DT), &mut sequence)
            .unwrap_err()
            .contains("changed scene setup")
    );
    assert_eq!(sequence, 1);
    runtime.execute_frame(time(DT));
    assert!(
        runtime
            .fire_scene_impulse(&fire, time(DT), &mut sequence)
            .unwrap()
    );
}

#[test]
fn scene_impulse_routes_report_exhaustion_and_recover_after_native_reset() {
    let (def, manifest) = scene_fixture(&["part_a"]);
    let fire = alias(&def, "fire");
    let mut runtime =
        PresetRuntime::from_def_for_render(def, &registry(), Some(&manifest), false).unwrap();
    runtime.execute_frame(time(0.0));
    let mut sequence = 0;
    for _ in 0..256 {
        runtime
            .fire_scene_impulse(&fire, time(0.0), &mut sequence)
            .unwrap();
    }
    assert!(
        runtime
            .fire_scene_impulse(&fire, time(0.0), &mut sequence)
            .unwrap_err()
            .contains("retained")
    );
    assert!(
        runtime
            .fire_scene_impulse(&fire, time(0.0), &mut sequence)
            .unwrap_err()
            .contains("previous admission failed")
    );
    edit(&mut runtime, "world", "reset", 1.0);
    runtime.execute_frame(time(0.0));
    runtime
        .fire_scene_impulse(&fire, time(0.0), &mut sequence)
        .unwrap();
    runtime.execute_frame(time(DT));
    let mut receipts = 0;
    runtime.drain_scene_impulses(|_, _| receipts += 1);
    assert_eq!(receipts, 1, "the reset cancels the exhausted epoch");
}

/// Audio fires arrive stamped with the frame time the engine just ticked to,
/// before that frame renders; manual Fire carries the last rendered time.
/// Rigid and CPU water re-observe at the source time, so both shapes land
/// once on the following ticks.
#[test]
fn scene_impulse_routes_accept_audio_hits_stamped_ahead_of_the_last_render() {
    let check = |with_fluid| {
        let (mut def, manifest) = scene_fixture(&["part_a", "part_b"]);
        if with_fluid {
            def.nodes.push(serde_json::from_value(serde_json::json!({"id":14,"nodeId":"fluid","typeId":manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID,
                "params":{"resolution":{"type":"Int","value":8},"fill_height":{"type":"Float","value":0.0},"emission":{"type":"Float","value":0.0}}})).unwrap());
            def.wires.retain(|wire| !(wire.to_node == 9 && wire.to_port == "transform"));
            def.wires.push(serde_json::from_value(
                serde_json::json!({"fromNode":14,"fromPort":"vertices","toNode":9,"toPort":"vertices"}),
            ).unwrap());
        }
        let fire = alias(&def, "fire");
        let mut runtime =
            PresetRuntime::from_def_for_render(def, &registry(), Some(&manifest), false).unwrap();
        runtime.execute_frame(time(0.0));
        runtime.execute_frame(time(DT));
        let mut sequence = 0;
        // Manual: the last rendered time.
        runtime.fire_scene_impulse(&fire, time(DT), &mut sequence).unwrap();
        // Audio: the next frame's time, before it renders.
        runtime.fire_scene_impulse(&fire, time(2.0 * DT), &mut sequence).unwrap();
        runtime.execute_frame(time(2.0 * DT));
        runtime.execute_frame(time(3.0 * DT));
        let mut sequences = Vec::new();
        runtime.drain_scene_impulses(|_, event| sequences.push(event.source.sequence));
        sequences.sort();
        assert_eq!(sequences, [0, 1], "fluid {with_fluid}: each hit applies once");
        // Older than the latest observation: stale, refused.
        assert!(runtime.fire_scene_impulse(&fire, time(2.0 * DT), &mut sequence).is_err());
    };
    check(false);
    #[cfg(feature = "gpu-proofs")]
    check(true);
}
