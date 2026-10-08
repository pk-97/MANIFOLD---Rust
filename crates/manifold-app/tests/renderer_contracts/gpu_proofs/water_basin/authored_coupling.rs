//! A saved scene's ordinary liquid-only Force must move its immersed rigid
//! object through the shared solver, without directly targeting that body.
use super::*;
use explicit_authoring::{author_scene, instrument, Observe, SAMPLE};
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, SerializedParamValue};
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
use manifold_core::{project::Project, Beats, GraphTarget, NodeId, Seconds};
use manifold_editing::command::Command;
use manifold_editing::commands::graph::{InsertSceneModifierCommand, SetGraphNodeParamCommand};
use manifold_node_engine::water::physics_events::ImpulseTarget;
use manifold_node_engine::exec::effect_node::FrameTime;

fn set_param(
    project: &mut Project,
    target: &GraphTarget,
    baseline: &EffectGraphDef,
    scope: &[u32],
    node: u32,
    name: &str,
    value: SerializedParamValue,
) {
    let mut edit =
        SetGraphNodeParamCommand::new(target.clone(), node, name.into(), value, baseline.clone())
            .with_scope(scope.to_vec());
    edit.execute(project);
    assert!(edit.was_applied(), "{:?}", edit.rejection_reason());
}

fn authored_vortex(viscosity: f32) -> (EffectGraphDef, NodeId, String) {
    let (mut project, target, baseline, body_id) = author_scene();
    let owner = project.graph_for_target(&target, None).unwrap().clone();
    let body_group = owner
        .nodes
        .iter()
        .find(|node| {
            node.group
                .as_ref()
                .is_some_and(|group| group.nodes.iter().any(|node| node.node_id == body_id))
        })
        .unwrap();
    let body_nodes = &body_group.group.as_ref().unwrap().nodes;
    let body = body_nodes
        .iter()
        .find(|node| node.node_id == body_id)
        .unwrap()
        .id;
    let mesh = body_nodes
        .iter()
        .find(|node| node.type_id == "node.cube_mesh")
        .unwrap()
        .id;
    let pose = body_nodes
        .iter()
        .find(|node| node.type_id == "node.transform_3d")
        .unwrap()
        .id;
    let liquid_group = owner
        .nodes
        .iter()
        .find(|node| {
            node.group.as_ref().is_some_and(|group| {
                group
                    .nodes
                    .iter()
                    .any(|node| node.type_id == manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID)
            })
        })
        .unwrap();
    let group = liquid_group.group.as_ref().unwrap();
    let fluid = group
        .nodes
        .iter()
        .find(|node| node.type_id == manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID)
        .unwrap()
        .id;
    let role = group
        .nodes
        .iter()
        .find(|node| node.type_id == "node.fluid_role_source")
        .unwrap()
        .id;
    let source = group
        .wires
        .iter()
        .find(|wire| wire.to_node == role && wire.to_port == "transform")
        .unwrap()
        .from_node;
    let domain = group
        .wires
        .iter()
        .find(|wire| wire.to_node == fluid && wire.to_port == "domain")
        .unwrap()
        .from_node;
    let liquid_object = group
        .nodes
        .iter()
        .find(|node| node.type_id == "node.scene_object")
        .unwrap()
        .node_id
        .clone();
    let world = owner
        .nodes
        .iter()
        .find(|node| node.type_id == "node.physics_world")
        .unwrap()
        .id;
    let scene = owner
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .unwrap()
        .node_id
        .clone();

    // This fixture intentionally has no gravity, isolating angular transfer
    // from the selected liquid. Edit the lifted World input through its node.
    let gravity = owner
        .wires
        .iter()
        .find(|wire| wire.to_node == world && wire.to_port == "gravity_y")
        .unwrap()
        .from_node;
    assert_eq!(
        owner
            .nodes
            .iter()
            .find(|node| node.id == gravity)
            .unwrap()
            .type_id,
        "node.value"
    );
    set_param(
        &mut project,
        &target,
        &baseline,
        &[],
        gravity,
        "value",
        SerializedParamValue::Float { value: 0.0 },
    );
    for (node, name, value) in [
        (mesh, "size", 0.5),
        (body, "density", 1000.0),
        (pose, "pos_y", 1.05),
        (pose, "scale_y", 0.8),
        (pose, "scale_z", 0.9),
    ] {
        set_param(
            &mut project,
            &target,
            &baseline,
            &[body_group.id],
            node,
            name,
            SerializedParamValue::Float { value },
        );
    }
    for (node, name, value) in [
        (fluid, "fill_height", 0.0),
        (fluid, "viscosity", viscosity),
        (source, "pos_y", 1.05),
        (source, "scale_x", 1.5),
        (source, "scale_y", 1.2),
        (source, "scale_z", 1.5),
        (domain, "pos_y", 1.2),
        (domain, "scale_x", 2.4),
        (domain, "scale_y", 2.4),
        (domain, "scale_z", 2.4),
        (role, "velocity_y", 0.0),
    ] {
        set_param(
            &mut project,
            &target,
            &baseline,
            &[liquid_group.id],
            node,
            name,
            SerializedParamValue::Float { value },
        );
    }
    set_param(
        &mut project,
        &target,
        &baseline,
        &[liquid_group.id],
        role,
        "role",
        SerializedParamValue::Enum { value: 0 },
    );

    let owner = project.graph_for_target(&target, None).unwrap();
    let mut recipe: EffectGraphDef = serde_json::from_str(manifold_nodes::testkit::assets::ASSETS_SCENE_MODIFIER_PRESETS_VORTEXFORCE_JSON)
    .unwrap();
    let metadata = recipe.preset_metadata.as_mut().unwrap();
    for (id, value) in [
        ("center_x", 0.15),
        ("center_y", 1.05),
        ("center_z", 0.0),
        ("axis_x", 0.0),
        ("axis_y", 0.0),
        ("axis_z", 1.0),
        ("radius", 0.9),
        ("falloff", 1.0),
        ("strength", 0.0),
        ("impulse_strength", 0.5),
    ] {
        metadata
            .params
            .iter_mut()
            .find(|param| param.id == id)
            .unwrap()
            .default_value = value;
        metadata
            .bindings
            .iter_mut()
            .find(|binding| binding.id == id)
            .unwrap()
            .default_value = value;
    }
    let force =
        manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
            owner,
            &recipe,
            NodeId::new("authored_vortex"),
            SceneNodeRef {
                scope: vec![],
                node: scene,
            },
            SceneTargetSelection::Explicit {
                objects: vec![SceneNodeRef {
                    scope: vec![liquid_group.node_id.clone()],
                    node: liquid_object,
                }],
            },
        )
        .unwrap();
    let mut insert =
        InsertSceneModifierCommand::new(&project, target.clone(), &baseline, 0, force).unwrap();
    insert.execute(&mut project);
    assert!(insert.was_applied(), "{:?}", insert.rejection_reason());
    let saved = serde_json::to_string(&project).unwrap();
    let restored: Project = serde_json::from_str(&saved).unwrap();
    let owner = restored.graph_for_target(&target, None).unwrap();
    let fire = owner.preset_metadata.as_ref().unwrap().bindings.iter().find(|binding|
        matches!(&binding.target, BindingTarget::SceneModifier { param_id, .. } if param_id == "fire")
    ).unwrap().id.clone();
    let mut graph = owner.clone();
    instrument(&mut graph, &body_id);
    (graph, body_id, fire)
}

#[test]
fn scene_physics_authored_liquid_only_vortex_rotates_immersed_object_after_reload() {
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let mut registry = PrimitiveRegistry::with_cpu_flip_reference();
    registry.register("test.explicit_physics_observer", || {
        Box::new(Observe(EffectNodeType::new(
            "test.explicit_physics_observer",
        )))
    });
    let target = RenderTarget::new(
        &harness.device,
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        "authored-coupling",
    );
    let _offline = PhysicsStepScope::for_render(true);
    for viscosity in [0.0, 1.0] {
        let (graph, _, fire) = authored_vortex(viscosity);
        let saved = serde_json::to_string(&graph).unwrap();
        let mut resting_roll = 0.0;
        for fired in [false, true] {
            let mut runtime = PresetRuntime::from_json_str_with_device(
                &saved,
                &registry,
                Arc::clone(&harness.device),
                WIDTH,
                HEIGHT,
                GpuTextureFormat::Rgba16Float,
                None,
            )
            .unwrap();
            warmup_mesh_roles(&mut runtime, &target, &harness.device);
            render_frame(&mut runtime, &target, &harness.device, 1);
            if fired {
                runtime
                    .fire_scene_impulse(
                        &fire,
                        FrameTime {
                            seconds: Seconds(1.0 / 60.0),
                            beats: Beats(1.0 / 60.0),
                            delta: Seconds::ZERO,
                            frame_count: 1,
                        },
                        &mut 0,
                    )
                    .unwrap();
            }
            let mut pixels = Vec::new();
            for frame in 2..=13 {
                pixels = render_frame(&mut runtime, &target, &harness.device, frame);
            }
            let accepted = SAMPLE.get();
            assert!((accepted.time - 13.0 / 60.0).abs() < 1e-6, "{accepted:?}");
            assert!((accepted.collider_width - 0.5).abs() < 1e-5, "{accepted:?}");
            assert!(
                accepted.particles > 0.0 && accepted.y.is_finite(),
                "{accepted:?}"
            );
            assert!(accepted.particles.is_finite(), "{accepted:?}");
            assert!(
                accepted.y.is_finite() && accepted.roll.is_finite(),
                "{accepted:?}"
            );
            assert!((0.0..=2.4).contains(&accepted.y), "{accepted:?}");
            assert_finite_and_nonempty(&pixels, 13);
            let mut receipts = 0;
            runtime.drain_scene_impulses(|_, event| {
                assert_eq!(event.value.target, ImpulseTarget::Fluid);
                receipts += 1;
            });
            assert_eq!(receipts, usize::from(fired));
            eprintln!("authored liquid vortex: viscosity={viscosity} fired={fired} {accepted:?}");
            if fired {
                assert!(
                    accepted.roll - resting_roll > 1e-4 && accepted.roll < 0.5,
                    "{accepted:?}"
                );
                let held = render_frame(&mut runtime, &target, &harness.device, 13);
                assert_pixels_close(&pixels, &held, 1e-3);
                assert_eq!(SAMPLE.get().roll, accepted.roll);
                runtime.drain_scene_impulses(|_, _| panic!("paused input must not fire again"));
                std::fs::write(
                    format!("/tmp/manifold_authored_vortex_{viscosity:.0}.png"),
                    readback_to_srgb_png(&harness.device, &target.texture, WIDTH, HEIGHT),
                )
                .unwrap();
            } else {
                resting_roll = accepted.roll;
                assert!(resting_roll.abs() < 1e-6, "{accepted:?}");
            }

            for frame in 14..=120 {
                pixels = render_frame(&mut runtime, &target, &harness.device, frame);
                let accepted = SAMPLE.get();
                assert!(
                    (accepted.time - frame as f32 / 60.0).abs() < 1e-5,
                    "viscosity={viscosity} fired={fired} frame={frame} {accepted:?}"
                );
                assert!(
                    accepted.particles.is_finite() && accepted.particles > 0.0,
                    "{accepted:?}"
                );
                assert!(
                    accepted.y.is_finite() && accepted.roll.is_finite(),
                    "{accepted:?}"
                );
                assert!((0.0..=2.4).contains(&accepted.y), "{accepted:?}");
                assert_finite_and_nonempty(&pixels, frame);
            }
            let final_sample = SAMPLE.get();
            eprintln!(
                "authored liquid vortex final: viscosity={viscosity} fired={fired} {final_sample:?}"
            );
            let held_final = render_frame(&mut runtime, &target, &harness.device, 120);
            assert_pixels_close(&pixels, &held_final, 1e-3);
            let held_sample = SAMPLE.get();
            assert_eq!(held_sample.time, final_sample.time);
            assert_eq!(held_sample.y, final_sample.y);
            assert_eq!(held_sample.roll, final_sample.roll);
            runtime.drain_scene_impulses(|_, _| panic!("final repeated frame must not fire again"));
            if fired {
                std::fs::write(
                    format!(
                        "/tmp/manifold_authored_vortex_viscosity_{viscosity:.0}_fired_frame120.png"
                    ),
                    readback_to_srgb_png(&harness.device, &target.texture, WIDTH, HEIGHT),
                )
                .unwrap();
            } else {
                resting_roll = final_sample.roll;
                assert!(resting_roll.abs() < 1e-6, "{final_sample:?}");
            }
        }
    }
}
