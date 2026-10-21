//! A viewport gizmo drag of a Fixed body while the show plays, followed from
//! the gizmo's command to the pose the running Physics World holds: the body
//! must follow within a tick of the edit, without a rebuild.
use super::*;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::Seconds;
use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, FrameTime};
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::ports::{NodeInput, NodeOutput};
use manifold_node_engine::runtime::PresetRuntime;
use manifold_nodes_scene::node_graph::scene_exposure::migrate_scene_exposures;
use manifold_water_rigid::primitives::physics_world::PhysicsWorldNode;

const SOLIDS: &str = manifold_nodes::testkit::assets::ASSETS_GENERATOR_PRESETS_PHYSICSSOLIDS_JSON;
const DT: f64 = 1.0 / 60.0;

/// The scene node with its raster presentation turned off; the physics
/// worlds and the graph are real.
struct CpuScene(manifold_nodes_scene::node_graph::primitives::render_scene::RenderScene);
impl EffectNode for CpuScene {
    fn type_id(&self) -> &EffectNodeType {
        self.0.type_id()
    }
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        self.0.depth_rule()
    }
    fn inputs(&self) -> &[NodeInput] {
        self.0.inputs()
    }
    fn outputs(&self) -> &[NodeOutput] {
        self.0.outputs()
    }
    fn parameters(&self) -> &[manifold_node_engine::parameters::ParamDef] {
        self.0.parameters()
    }
    fn reconfigure(&mut self, params: &manifold_node_engine::exec::effect_node::ParamValues) {
        self.0.reconfigure(params);
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}

fn registry() -> PrimitiveRegistry {
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register("node.render_scene", || {
        Box::new(CpuScene(manifold_nodes_scene::node_graph::primitives::render_scene::RenderScene::new()))
    });
    registry
}

fn time(seconds: f64) -> FrameTime {
    FrameTime { seconds: Seconds(seconds), beats: manifold_core::Beats(seconds * 2.0), delta: Seconds::ZERO, frame_count: 0 }
}

fn solids_project(bound: bool) -> (Project, LayerId) {
    let mut project = Project::default();
    let index = project.timeline.add_layer("Solids", LayerType::Generator, PresetTypeId::from_string("PhysicsSolids".to_string()));
    let layer_id = project.timeline.layers[index].layer_id.clone();
    let mut def: EffectGraphDef = serde_json::from_str(SOLIDS).unwrap();
    if bound {
        migrate_scene_exposures(&mut def);
    }
    let generator = project.timeline.layers[index].gen_params_mut().unwrap();
    generator.graph = Some(def);
    generator.refresh_manifest_from_graph();
    (project, layer_id)
}

/// What `GeneratorRenderer::render_all` does with one layer each frame: a
/// structure change rebuilds and carries state, a value change lands in
/// place, then the card values drive the bindings. The scene node is real
/// only when the device is (`gpu-proofs`).
struct Live {
    runtime: PresetRuntime,
    structure: u32,
    values: u32,
    rebuilds: usize,
    #[cfg(feature = "gpu-proofs")]
    target: Option<manifold_node_engine::gpu::render_target::RenderTarget>,
}

impl Live {
    fn new(project: &Project, layer: &LayerId) -> Self {
        let layer = project.timeline.layers.iter().find(|l| &l.layer_id == layer).unwrap();
        let gp = layer.gen_params().unwrap();
        let runtime = PresetRuntime::from_def_for_render(gp.graph.clone().unwrap(), &registry(), Some(&gp.params), false).unwrap();
        Self {
            runtime,
            structure: layer.generator_graph_structure_version(),
            values: layer.generator_graph_version(),
            rebuilds: 0,
            #[cfg(feature = "gpu-proofs")]
            target: None,
        }
    }

    #[cfg(feature = "gpu-proofs")]
    fn new_on_gpu(project: &Project, layer: &LayerId) -> Self {
        let h = manifold_node_engine::testkit::gpu_harness::shared();
        let layer = project.timeline.layers.iter().find(|l| &l.layer_id == layer).unwrap();
        let gp = layer.gen_params().unwrap();
        let runtime = Self::gpu_runtime(gp);
        Self {
            runtime,
            structure: layer.generator_graph_structure_version(),
            values: layer.generator_graph_version(),
            rebuilds: 0,
            target: Some(h.make_target("body-drag")),
        }
    }

    #[cfg(feature = "gpu-proofs")]
    fn gpu_runtime(gp: &manifold_core::effects::PresetInstance) -> PresetRuntime {
        let h = manifold_node_engine::testkit::gpu_harness::shared();
        let def = gp.graph.as_ref().unwrap();
        manifold_nodes::registry::GeneratorRegistry::new(manifold_gpu::GpuTextureFormat::Rgba16Float)
            .create_with_override(h.device.clone(), &def.preset_metadata.as_ref().unwrap().id, Some(def), h.width, h.height, false, Some(&gp.params))
            .map(|runtime| *runtime)
            .expect("the graph builds")
    }

    fn frame(&mut self, project: &Project, layer: &LayerId, frame: usize) {
        let layer = project.timeline.layers.iter().find(|l| &l.layer_id == layer).unwrap();
        let gp = layer.gen_params().unwrap();
        if layer.generator_graph_structure_version() != self.structure {
            #[cfg(feature = "gpu-proofs")]
            let mut next = if self.target.is_some() {
                Self::gpu_runtime(gp)
            } else {
                PresetRuntime::from_def_for_render(gp.graph.clone().unwrap(), &registry(), Some(&gp.params), false).unwrap()
            };
            #[cfg(not(feature = "gpu-proofs"))]
            let mut next = PresetRuntime::from_def_for_render(gp.graph.clone().unwrap(), &registry(), Some(&gp.params), false).unwrap();
            next.carry_generator_state_from(&mut self.runtime);
            self.runtime = next;
            self.structure = layer.generator_graph_structure_version();
            self.values = layer.generator_graph_version();
            self.rebuilds += 1;
        } else if layer.generator_graph_version() != self.values {
            self.runtime.apply_inner_param_overrides(gp.graph.as_ref().unwrap());
            self.runtime.apply_manifest_reshape(&gp.params, gp.graph.as_ref());
            self.values = layer.generator_graph_version();
        }
        #[cfg(feature = "gpu-proofs")]
        if let Some(target) = &self.target {
            use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
            let h = manifold_node_engine::testkit::gpu_harness::shared();
            let seconds = frame as f64 * DT;
            let context = manifold_node_engine::runtime::preset_context::PresetContext {
                time: seconds,
                beat: seconds,
                dt: DT as f32,
                width: h.width,
                height: h.height,
                output_width: h.width,
                output_height: h.height,
                aspect: 1.0,
                owner_key: 0,
                is_clip_level: false,
                frame_count: frame as i64,
                anim_progress: 0.0,
                trigger_count: 0,
            };
            let mut encoder = h.device.create_encoder("body-drag");
            {
                let mut gpu = RendererGpuEncoder::new(&mut encoder, &h.device);
                self.runtime.render(&mut gpu, &target.texture, &context, &gp.params);
            }
            encoder.commit_and_wait_completed();
            return;
        }
        self.runtime.apply_param_values(&gp.params);
        self.runtime.execute_frame(time(frame as f64 * DT));
    }

    fn poses(&self) -> Vec<manifold_node_engine::scene::transform::Transform> {
        self.world().simulation.poses.clone()
    }

    fn world(&self) -> &PhysicsWorldNode {
        let world = self.runtime.graph.nodes().find(|n| n.node.type_id().as_str() == "node.physics_world").expect("a physics world");
        world.node.as_any().downcast_ref::<PhysicsWorldNode>().unwrap()
    }
}

/// Peter's report: a Fixed body dragged with the viewport gizmo while the
/// show plays does not move in the world until physics resets.
fn drag_the_ground(bound: bool) {
    let (mut local, layer) = solids_project(bound);
    let mut content = local.clone();
    let mut live = Live::new(&content, &layer);
    for frame in 0..30 {
        live.frame(&content, &layer, frame);
    }
    let before = live.poses();

    let scene = scene(&content, &layer);
    let ground = gizmo_targets(&scene)
        .filter(|t| t.kind == GizmoTargetKind::Object)
        .min_by(|a, b| a.origin[1].partial_cmp(&b.origin[1]).unwrap())
        .expect("the ground is an object target");
    let mut rig = Rig::new(&layer);
    rig.drag(&mut local, &layer, &ground, GizmoMode::Move, GizmoAxis::X, px(ground.origin), px(plus(ground.origin, [2.0, 0.0, 0.0])));
    let mut service = EditingService::new();
    rig.apply(&mut service, &mut content);
    for frame in 30..34 {
        live.frame(&content, &layer, frame);
    }
    let after = live.poses();
    assert!(after[0].pos[0] - before[0].pos[0] > 1.0, "bound {bound}: the ground moved in the world: {:?} -> {:?}", before[0].pos, after[0].pos);
    assert_eq!(live.rebuilds, 0, "bound {bound}: no rebuild");
    assert!(after[1].pos[1] < before[1].pos[1], "bound {bound}: the falling solid kept falling");
}

#[test]
fn a_gizmo_drag_of_a_fixed_body_moves_it_in_the_running_world_through_its_card() {
    drag_the_ground(true);
}

#[test]
fn a_gizmo_drag_of_a_fixed_body_moves_it_in_the_running_world_through_its_node_param() {
    drag_the_ground(false);
}

/// An imported photoscan with Enable Physics on and its body Fixed, playing on
/// the real device, then one gizmo drag of the object target `pick` chooses.
/// Returns what the world did.
#[cfg(feature = "gpu-proofs")]
struct Dragged {
    before: Vec<manifold_node_engine::scene::transform::Transform>,
    after: Vec<manifold_node_engine::scene::transform::Transform>,
    /// Renderer rebuilds of the generator.
    rebuilds: usize,
    /// The world's reset epoch before and after.
    epochs: (Option<u64>, Option<u64>),
}

#[cfg(feature = "gpu-proofs")]
fn drag_the_photoscan(pick: impl Fn(&[GizmoTarget]) -> GizmoTarget) -> Dragged {
    use manifold_core::effect_graph_def::{BindingTarget, SerializedParamValue};
    use manifold_core::GraphTarget;
    use manifold_editing::command::Command;
    use manifold_editing::commands::graph::EnableSceneObjectPhysicsCommand;
    use manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type;

    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf/cc0__tiger_lily.glb");
    let (imported_graph, _) = manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph(&fixture).expect("the photoscan imports");
    let mut project = Project::default();
    let preset_id = imported_graph.preset_metadata.as_ref().unwrap().id.clone();
    let index = project.timeline.add_layer("Photoscan", LayerType::Generator, preset_id);
    project.timeline.layers[index].gen_params_mut().unwrap().graph = Some(imported_graph);
    let layer = project.timeline.layers[0].layer_id.clone();
    let imported = authored_def(&project, &GraphTarget::Generator(layer.clone())).unwrap();
    let render = imported.nodes.iter().find(|n| n.type_id == "node.render_scene").expect("render scene").id;
    let mut enable = EnableSceneObjectPhysicsCommand::new(
        &project,
        GraphTarget::Generator(layer.clone()),
        render,
        0,
        &metadata_for_node_type("node.rigid_body"),
        &metadata_for_node_type("node.physics_world"),
        &imported,
    )
    .expect("the import takes physics");
    enable.execute(&mut project);
    assert!(enable.was_applied(), "enable rejected: {:?}", enable.rejection_reason());

    // Fixed, through the Motion card as the app writes it.
    let generator = project.timeline.layers[0].gen_params_mut().unwrap();
    let def = generator.graph.as_mut().unwrap();
    let mut bodies = Vec::new();
    for group in def.nodes.iter_mut().filter_map(|n| n.group.as_mut()) {
        for body in group.nodes.iter_mut().filter(|n| n.type_id == "node.rigid_body") {
            body.params.insert("motion".into(), SerializedParamValue::Enum { value: 0 });
            bodies.push(body.node_id.clone());
        }
    }
    let cards: Vec<String> = def
        .preset_metadata
        .iter()
        .flat_map(|m| m.bindings.iter())
        .filter(|b| matches!(&b.target, BindingTarget::Node { node_id, param } if param == "motion" && bodies.contains(node_id)))
        .map(|b| b.id.clone())
        .collect();
    generator.refresh_manifest_from_graph();
    for card in &cards {
        generator.set_base_param(card, 0.0);
    }

    let mut local = project.clone();
    let mut content = project;
    let mut live = Live::new_on_gpu(&content, &layer);
    let wait = manifold_node_engine::testkit::gpu_harness::BackgroundWait::new("photoscan warmup");
    let mut frame = 0;
    for _ in 0..60 {
        live.frame(&content, &layer, frame);
        frame += 1;
        while live.runtime.warmup_pending() {
            wait.hold();
            live.frame(&content, &layer, frame);
        }
    }
    assert!(live.runtime.errors().is_empty(), "photoscan errors: {:?}", live.runtime.errors());
    let before = live.poses();
    let epoch_before = live.world().simulation.impulse_epoch();

    let scene = scene(&content, &layer);
    let targets: Vec<GizmoTarget> = gizmo_targets(&scene).filter(|t| t.kind == GizmoTargetKind::Object).collect();
    let target = pick(&targets);
    let mut rig = Rig::new(&layer);
    rig.drag(&mut local, &layer, &target, GizmoMode::Move, GizmoAxis::X, px(target.origin), px(plus(target.origin, [2.0, 0.0, 0.0])));
    let mut service = EditingService::new();
    rig.apply(&mut service, &mut content);
    for _ in 0..6 {
        live.frame(&content, &layer, frame);
        frame += 1;
        while live.runtime.warmup_pending() {
            wait.hold();
            live.frame(&content, &layer, frame);
        }
    }
    Dragged { before, after: live.poses(), rebuilds: live.rebuilds, epochs: (epoch_before, live.world().simulation.impulse_epoch()) }
}

/// The object's own gizmo moves the group transform that places the body.
#[cfg(feature = "gpu-proofs")]
#[test]
fn a_gizmo_drag_of_a_fixed_imported_photoscan_moves_it_in_the_running_world() {
    let dragged = drag_the_photoscan(|targets| targets[0].clone());
    let (before, after) = (&dragged.before[0].pos, &dragged.after[0].pos);
    assert!(after[0] - before[0] > 1.0, "the photoscan moved in the world: {before:?} -> {after:?}");
    assert_eq!(dragged.rebuilds, 0, "no renderer rebuild");
    assert_eq!(dragged.epochs.0, dragged.epochs.1, "the world was not reset");
}

/// A part of the photoscan picked in the viewport moves its collider part
/// offset, which is cooked into the body's collider.
#[cfg(feature = "gpu-proofs")]
#[test]
fn a_gizmo_drag_of_one_photoscan_part_reaches_the_world_without_a_reset() {
    let dragged = drag_the_photoscan(|targets| targets[1].clone());
    assert_eq!(dragged.rebuilds, 0, "no renderer rebuild");
    assert_eq!(dragged.epochs.0, dragged.epochs.1, "the world was not reset");
}
