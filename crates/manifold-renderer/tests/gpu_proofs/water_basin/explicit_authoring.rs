//! Ordinary object authoring must reach the shared native scene without a
//! special physics mesh or a creation-order dependency.
use super::*;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_core::{GraphTarget, PresetTypeId, layer::Layer, project::Project};
use manifold_editing::command::Command;
use manifold_editing::commands::graph::{
    AddSceneFluidCommand, AddSceneObjectCommand, EnableSceneObjectPhysicsCommand,
    SetGraphNodeParamCommand,
};
use manifold_renderer::node_graph::scene_vm::{SceneObjectVm, SceneVm};
use manifold_renderer::node_graph::{bundled_preset_def, scene_exposure::metadata_for_node_type};

#[derive(Clone, Copy, Debug, Default)]
struct Sample {
    time: f32,
    y: f32,
    particles: f32,
    collider_width: f32,
}
thread_local! { static SAMPLE: Cell<Sample> = const { Cell::new(Sample {
    time: 0.0, y: 0.0, particles: 0.0, collider_width: 0.0,
}) }; }

struct Observe(EffectNodeType);
impl EffectNode for Observe {
    fn type_id(&self) -> &EffectNodeType {
        &self.0
    }
    fn depth_rule(&self) -> DepthRule {
        DepthRule::Terminal
    }
    fn is_liveness_root(&self) -> bool {
        true
    }
    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 4] = [
            NodePort {
                name: std::borrow::Cow::Borrowed("body"),
                ty: PortType::RigidBody,
                kind: PortKind::Input,
                required: true,
            },
            NodePort {
                name: std::borrow::Cow::Borrowed("pose"),
                ty: PortType::Transform,
                kind: PortKind::Input,
                required: true,
            },
            NodePort {
                name: std::borrow::Cow::Borrowed("time"),
                ty: PortType::Scalar(manifold_renderer::node_graph::ports::ScalarType::F32),
                kind: PortKind::Input,
                required: true,
            },
            NodePort {
                name: std::borrow::Cow::Borrowed("particles"),
                ty: PortType::Scalar(manifold_renderer::node_graph::ports::ScalarType::F32),
                kind: PortKind::Input,
                required: true,
            },
        ];
        &INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (
            Some(body),
            Some(pose),
            Some(ParamValue::Float(time)),
            Some(ParamValue::Float(particles)),
        ) = (
            ctx.inputs.rigid_body("body"),
            ctx.inputs.transform("pose"),
            ctx.inputs.scalar("time"),
            ctx.inputs.scalar("particles"),
        ) else {
            ctx.mark_outputs_pending();
            return;
        };
        let collider = body.collider.as_ref().expect("wired source collider");
        assert_eq!(collider.hulls.len(), 1, "cube has one exact hull");
        let min = collider.hulls[0]
            .iter()
            .map(|point| point[0])
            .fold(f32::INFINITY, f32::min);
        let max = collider.hulls[0]
            .iter()
            .map(|point| point[0])
            .fold(f32::NEG_INFINITY, f32::max);
        SAMPLE.set(Sample {
            time,
            y: pose.pos[1],
            particles,
            collider_width: max - min,
        });
    }
}

fn set(
    project: &mut Project,
    target: &GraphTarget,
    baseline: &EffectGraphDef,
    scope: Vec<u32>,
    node: u32,
    name: &str,
    value: f32,
) {
    let mut command = SetGraphNodeParamCommand::new(
        target.clone(),
        node,
        name.into(),
        SerializedParamValue::Float { value },
        baseline.clone(),
    )
    .with_scope(scope);
    command.execute(project);
    assert!(command.was_applied(), "{:?}", command.rejection_reason());
}

#[test]
fn scene_physics_explicit_object_uses_shared_world_after_fluid_authoring() {
    let mut project = Project::default();
    let preset = PresetTypeId::new("SceneStarter");
    let baseline = bundled_preset_def(&preset).unwrap();
    let scene = baseline
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .unwrap()
        .id;
    let layer = Layer::new_generator("Explicit Physics".into(), preset, 0);
    let target = GraphTarget::Generator(layer.layer_id.clone());
    project.timeline.layers.push(layer);
    let mut fluid = AddSceneFluidCommand::new(
        target.clone(),
        scene,
        metadata_for_node_type("node.fluid_surface"),
        metadata_for_node_type("node.transform_3d"),
        metadata_for_node_type("node.pbr_material"),
        metadata_for_node_type("node.scene_object"),
        baseline.clone(),
    )
    .with_role_metadata(metadata_for_node_type("node.fluid_role_source"))
    .with_world_metadata(metadata_for_node_type("node.physics_world"));
    fluid.execute(&mut project);
    assert!(fluid.was_applied(), "{:?}", fluid.rejection_reason());
    let mut object = AddSceneObjectCommand::new(
        target.clone(),
        vec![],
        scene,
        0,
        (0.0, 0.0),
        metadata_for_node_type("node.phong_material"),
        metadata_for_node_type("node.transform_3d"),
        metadata_for_node_type("node.scene_object"),
        baseline.clone(),
    );
    object.execute(&mut project);
    assert!(object.was_applied());
    let def = project.graph_for_target(&target, None).unwrap();
    let row = SceneVm::from_def(def)
        .unwrap()
        .objects
        .into_iter()
        .filter_map(|object| match object {
            SceneObjectVm::Known(row)
                if row.group_node_id.is_some() && row.fluid_node_ids.is_empty() =>
            {
                Some(row)
            }
            _ => None,
        })
        .max_by_key(|row| row.index)
        .unwrap();
    assert!(
        row.physics.is_none(),
        "World presence must not choose an object's role"
    );
    let group_id = row.group_node_id.unwrap();
    let group = def
        .nodes
        .iter()
        .find(|node| node.id == group_id)
        .unwrap()
        .group
        .as_ref()
        .unwrap();
    let mesh = group
        .nodes
        .iter()
        .find(|node| node.type_id == "node.cube_mesh")
        .unwrap()
        .id;
    let transform = group
        .nodes
        .iter()
        .find(|node| node.type_id == "node.transform_3d")
        .unwrap()
        .id;
    set(
        &mut project,
        &target,
        &baseline,
        vec![group_id],
        mesh,
        "size",
        0.6,
    );
    set(
        &mut project,
        &target,
        &baseline,
        vec![group_id],
        transform,
        "pos_y",
        2.4,
    );
    let mut enable = EnableSceneObjectPhysicsCommand::new(
        target.clone(),
        scene,
        row.index as u32,
        metadata_for_node_type("node.rigid_body"),
        baseline.clone(),
    )
    .with_world_metadata(metadata_for_node_type("node.physics_world"));
    enable.execute(&mut project);
    assert!(enable.was_applied(), "{:?}", enable.rejection_reason());
    let def = project.graph_for_target(&target, None).unwrap();
    assert_eq!(
        def.nodes
            .iter()
            .filter(|node| node.type_id == "node.physics_world")
            .count(),
        1
    );
    let body_id = def
        .nodes
        .iter()
        .find(|node| node.id == group_id)
        .unwrap()
        .group
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|node| node.type_id == "node.rigid_body")
        .unwrap()
        .node_id
        .clone();
    let saved = serde_json::to_string(&project).unwrap();
    let restored: Project = serde_json::from_str(&saved).unwrap();
    // Flatten only for test instrumentation; the saved scene retains ordinary
    // groups, exposure identities and commands.
    let mut def =
        manifold_core::flatten::flatten_groups(restored.graph_for_target(&target, None).unwrap())
            .unwrap();
    let body = def
        .nodes
        .iter()
        .find(|node| node.node_id == body_id)
        .unwrap()
        .id;
    let world = def
        .nodes
        .iter()
        .find(|node| node.type_id == "node.physics_world")
        .unwrap()
        .id;
    let liquid = def
        .nodes
        .iter()
        .find(|node| node.type_id == "node.fluid_surface")
        .unwrap()
        .id;
    let slot = def
        .wires
        .iter()
        .find(|wire| wire.from_node == body && wire.to_node == world)
        .unwrap()
        .to_port
        .strip_prefix("body_")
        .unwrap()
        .to_owned();
    let observer = def.nodes.iter().map(|node| node.id).max().unwrap() + 1;
    def.nodes.push(serde_json::from_value::<EffectGraphNode>(serde_json::json!({
        "id": observer, "nodeId": "explicit_physics_observer", "typeId": "test.explicit_physics_observer"
    })).unwrap());
    for (from_node, from_port, to_port) in [
        (body, "body".to_owned(), "body"),
        (world, format!("pose_{slot}"), "pose"),
        (liquid, "simulation_time".to_owned(), "time"),
        (liquid, "particle_count".to_owned(), "particles"),
    ] {
        def.wires.push(EffectGraphWire {
            from_node,
            from_port,
            to_node: observer,
            to_port: to_port.into(),
        });
    }
    let harness = harness::shared();
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register("test.explicit_physics_observer", || {
        Box::new(Observe(EffectNodeType::new(
            "test.explicit_physics_observer",
        )))
    });
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &serde_json::to_string(&def).unwrap(),
        &registry,
        Arc::clone(&harness.device),
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap();
    let output = RenderTarget::new(
        &harness.device,
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        "explicit-physics-authoring",
    );
    let _offline = PhysicsStepScope::for_render(true);
    warmup_mesh_roles(&mut runtime, &output, &harness.device);
    let initial = SAMPLE.get();
    assert!((initial.collider_width - 0.6).abs() < 1e-5, "{initial:?}");
    let mut pixels = Vec::new();
    for frame in 1..=6 {
        pixels = render_frame(&mut runtime, &output, &harness.device, frame);
    }
    let advanced = SAMPLE.get();
    assert!((advanced.time - 6.0 / 60.0).abs() < 1e-6, "{advanced:?}");
    assert!(
        advanced.particles > 0.0,
        "authored domain contains liquid: {advanced:?}"
    );
    assert!(
        advanced.y < initial.y - 1e-3,
        "explicit body responds to World gravity: {initial:?} -> {advanced:?}"
    );
    assert_finite_and_nonempty(&pixels, 6);
    let held = render_frame(&mut runtime, &output, &harness.device, 6);
    assert_pixels_close(&pixels, &held, 1e-3);
    assert_eq!(SAMPLE.get().y, advanced.y);
    std::fs::write(
        "/tmp/manifold_explicit_physics.png",
        readback_to_srgb_png(&harness.device, &output.texture, WIDTH, HEIGHT),
    )
    .unwrap();
}
