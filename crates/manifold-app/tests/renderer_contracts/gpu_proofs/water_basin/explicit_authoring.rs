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
use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectVm, SceneVm};
use manifold_node_engine::water::physics::RigidBody;
use {manifold_nodes::bundled_presets::bundled_preset_def, manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type};

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Sample {
    pub(super) time: f32,
    pub(super) y: f32,
    pub(super) particles: f32,
    pub(super) collider_width: f32,
    pub(super) roll: f32,
}
thread_local! { pub(super) static SAMPLE: Cell<Sample> = const { Cell::new(Sample {
    time: 0.0, y: 0.0, particles: 0.0, collider_width: 0.0, roll: 0.0,
}) }; }

pub(super) struct Observe(pub(super) EffectNodeType);
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
                ty: PortType::Scalar(manifold_node_engine::ports::ScalarType::F32),
                kind: PortKind::Input,
                required: true,
            },
            NodePort {
                name: std::borrow::Cow::Borrowed("particles"),
                ty: PortType::Scalar(manifold_node_engine::ports::ScalarType::F32),
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
            ctx.inputs.cpu_value::<RigidBody>("body"),
            ctx.inputs.transform("pose"),
            ctx.inputs.scalar("time"),
            ctx.inputs.scalar("particles"),
        )
        else {
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
            roll: pose.rot_euler[2],
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

pub(super) fn author_scene() -> (Project, GraphTarget, EffectGraphDef, manifold_core::NodeId) {
    let mut project = Project::default();
    let preset = PresetTypeId::new("Scene");
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
        manifold_nodes::testkit::reference_fixtures::cpu_flip_metadata(),
        metadata_for_node_type("node.transform_3d"),
        metadata_for_node_type("node.pbr_material"),
        metadata_for_node_type("node.scene_object"),
        manifold_editing::commands::graph::flip_scene_fluid_template(),
        baseline.as_ref().clone(),
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
        metadata_for_node_type("node.pbr_material"),
        metadata_for_node_type("node.transform_3d"),
        metadata_for_node_type("node.scene_object"),
        baseline.as_ref().clone(),
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
                if row.group_node_id.is_some() && row.fluid_controls.is_empty() =>
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
        baseline.as_ref(),
        vec![group_id],
        mesh,
        "size",
        0.6,
    );
    set(
        &mut project,
        &target,
        baseline.as_ref(),
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
        baseline.as_ref().clone(),
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
    (project, target, baseline.as_ref().clone(), body_id)
}

#[test]
fn scene_physics_explicit_object_uses_shared_world_after_fluid_authoring() {
    let (project, target, _, body_id) = author_scene();
    let saved = serde_json::to_string(&project).unwrap();
    let restored: Project = serde_json::from_str(&saved).unwrap();
    // Flatten only for test instrumentation; the saved scene retains ordinary
    // groups, exposure identities and commands.
    let mut def =
        manifold_core::flatten::flatten_groups(restored.graph_for_target(&target, None).unwrap())
            .unwrap();
    instrument(&mut def, &body_id);
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let mut registry = PrimitiveRegistry::with_cpu_flip_reference();
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

/// Observe accepted output through existing group boundaries, retaining the
/// authored identities used by scene-modifier expansion.
pub(super) fn instrument(def: &mut EffectGraphDef, body_id: &manifold_core::NodeId) {
    fn expose(
        def: &mut EffectGraphDef,
        identity: &manifold_core::NodeId,
        port: &str,
        ty: &str,
    ) -> (u32, String) {
        use manifold_core::effect_graph_def::{GROUP_OUTPUT_TYPE_ID, InterfacePortDef};
        for node in &mut def.nodes {
            if &node.node_id == identity {
                return (node.id, port.into());
            }
            if let Some(group) = &mut node.group
                && let Some(inner) = group.nodes.iter().find(|inner| &inner.node_id == identity)
            {
                let inner_id = inner.id;
                let output = group
                    .nodes
                    .iter()
                    .find(|inner| inner.type_id == GROUP_OUTPUT_TYPE_ID)
                    .unwrap()
                    .id;
                let name = format!("test_{port}");
                group.interface.outputs.push(InterfacePortDef {
                    name: name.clone(),
                    port_type: ty.into(),
                });
                group.wires.push(EffectGraphWire {
                    from_node: inner_id,
                    from_port: port.into(),
                    to_node: output,
                    to_port: name.clone(),
                });
                return (node.id, name);
            }
        }
        panic!("missing observed node {identity:?}");
    }
    let world = def
        .nodes
        .iter()
        .find(|node| node.type_id == "node.physics_world")
        .unwrap()
        .id;
    let liquid_id = def
        .nodes
        .iter()
        .chain(
            def.nodes
                .iter()
                .filter_map(|node| node.group.as_ref())
                .flat_map(|group| &group.nodes),
        )
        .find(|node| node.type_id == manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID)
        .unwrap()
        .node_id
        .clone();
    let body = expose(def, body_id, "body", "RigidBody");
    let time = expose(def, &liquid_id, "simulation_time", "Scalar(F32)");
    let particles = expose(def, &liquid_id, "particle_count", "Scalar(F32)");
    let slot = def
        .wires
        .iter()
        .find(|wire| wire.from_node == body.0 && wire.to_node == world)
        .unwrap()
        .to_port
        .strip_prefix("body_")
        .unwrap()
        .to_owned();
    let observer = def
        .nodes
        .iter()
        .chain(
            def.nodes
                .iter()
                .filter_map(|node| node.group.as_ref())
                .flat_map(|group| &group.nodes),
        )
        .map(|node| node.id)
        .max()
        .unwrap()
        + 1;
    def.nodes.push(serde_json::from_value::<EffectGraphNode>(serde_json::json!({
        "id": observer, "nodeId": "explicit_physics_observer", "typeId": "test.explicit_physics_observer"
    })).unwrap());
    for (from_node, from_port, to_port) in [
        (body.0, body.1, "body"),
        (world, format!("pose_{slot}"), "pose"),
        (time.0, time.1, "time"),
        (particles.0, particles.1, "particles"),
    ] {
        def.wires.push(EffectGraphWire {
            from_node,
            from_port,
            to_node: observer,
            to_port: to_port.into(),
        });
    }
}
