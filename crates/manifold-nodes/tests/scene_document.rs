//! A scene document through save, load and a running world
//! (SCENE_DOCUMENT_DESIGN.md section 7 (Commands and the compiler)): a saved
//! scene loads back as the same scene, adding a light keeps a falling body
//! falling, and a Speed past its range clamps instead of freezing the world.

use std::borrow::Cow;
use std::cell::Cell;

use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};
use manifold_core::effects::{ParameterDriver, PresetInstance};
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::scene_doc::{Item, ItemFields, ItemId, ItemKind, PhysicsSetting, SceneDoc};
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_core::{Beats, PresetTypeId, Seconds};
use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, FrameTime};
use manifold_node_engine::parameters::ParamDef;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::scene::transform::Transform;

fn scene_layer(doc: SceneDoc) -> Layer {
    let mut layer = Layer::new_generator("Scene".into(), PresetTypeId::from_string("Scene".into()), 0);
    layer.gen_params_mut().unwrap().install_scene_document(doc).unwrap();
    layer
}

/// Every param's id, value and base.
fn values(fx: &PresetInstance) -> Vec<(String, f32, f32)> {
    fx.params.iter().map(|p| (p.id().to_owned(), p.value, p.base)).collect()
}

#[test]
fn a_saved_scene_loads_back_as_the_same_scene() {
    let mut doc = SceneDoc::base();
    doc.insert(None, 0, Item::new(ItemId::new("rig"), "Rig", ItemFields::default_for(ItemKind::Group))).unwrap();
    doc.reparent(&ItemId::new("cube"), Some(&ItemId::new("rig")), 0).unwrap();
    let mut layer = scene_layer(doc.clone());
    let fx = layer.gen_params_mut().unwrap();
    assert!(fx.set_base_param("cube.pos_y", 2.5), "the cube has a Position Y");
    fx.drivers = Some(vec![ParameterDriver::new("cube.pos_y", BeatDivision::Quarter, DriverWaveform::Sine)]);
    let (before, graph) = (values(fx), fx.graph.clone());
    let mut project = Project::default();
    project.timeline.layers.push(layer);

    let saved = serde_json::to_string(&project).unwrap();
    let mut loaded: Project = serde_json::from_str(&saved).unwrap();
    loaded.on_after_deserialize();
    loaded.reconcile_param_manifests();

    let fx = loaded.timeline.layers[0].gen_params().unwrap();
    assert_eq!(fx.scene.as_ref(), Some(&doc), "the document comes back");
    assert!(graph.is_some() && fx.graph == graph, "and compiles to the same graph");
    assert_eq!(values(fx), before, "every value comes back under its id");
    assert_eq!(fx.drivers.as_ref().unwrap()[0].param_id, "cube.pos_y", "the LFO still points at the cube");
}

thread_local! {
    static POSE: Cell<Option<Transform>> = const { Cell::new(None) };
}

struct PoseProbe(EffectNodeType);

impl EffectNode for PoseProbe {
    fn is_liveness_root(&self) -> bool {
        true
    }
    fn runs_with_pending_inputs(&self) -> bool {
        true
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.0
    }
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }
    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 1] =
            [NodePort { name: Cow::Borrowed("pose"), ty: PortType::Transform, kind: PortKind::Input, required: true }];
        &INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        POSE.set(ctx.inputs.transform("pose"));
    }
}

/// The compiled scene with the cube's body joining the world: the part a
/// later lane emits, stood in for by the rigid test body fed from the cube's own
/// transform, and a probe reading its pose.
fn with_cube_body(mut def: EffectGraphDef) -> EffectGraphDef {
    let id = |def: &EffectGraphDef, node_id: &str| def.nodes.iter().find(|n| n.node_id.as_str() == node_id).unwrap().id;
    let (transform, world) = (id(&def, "cube:transform"), id(&def, "world:physics"));
    let next = def.nodes.iter().map(|n| n.id).max().unwrap() + 1;
    for (offset, node_id, type_id) in [(0, "cube:body", "test.physics_body"), (1, "probe", "test.scene_pose")] {
        let node: EffectGraphNode =
            serde_json::from_value(serde_json::json!({"id": next + offset, "nodeId": node_id, "typeId": type_id})).unwrap();
        def.nodes.push(node);
    }
    for (from_node, from_port, to_node, to_port) in
        [(transform, "transform", next, "transform"), (next, "body", world, "body_0"), (world, "pose_0", next + 1, "pose")]
    {
        def.wires.push(EffectGraphWire { from_node, from_port: from_port.into(), to_node, to_port: to_port.into() });
    }
    def
}

fn runtime(fx: &PresetInstance) -> PresetRuntime {
    let mut registry = PrimitiveRegistry::with_builtin();
    manifold_water_rigid::testkit::physics_fixtures::register(&mut registry);
    registry.register("test.scene_pose", || Box::new(PoseProbe(EffectNodeType::new("test.scene_pose"))));
    let def = with_cube_body(fx.graph.clone().unwrap());
    let mut runtime = PresetRuntime::from_def_for_render(def, &registry, Some(&fx.params), false).unwrap();
    runtime.apply_param_values(&fx.params);
    runtime
}

fn frame(runtime: &mut PresetRuntime, seconds: f64) -> Transform {
    POSE.set(None);
    runtime.execute_frame(FrameTime {
        seconds: Seconds(seconds),
        beats: Beats(seconds * 2.0),
        delta: Seconds(1.0 / 30.0),
        frame_count: (seconds * 30.0) as i64,
    });
    POSE.get().expect("the world publishes the cube's pose")
}

/// Adding a light is a structural edit, so the scene recompiles and the
/// generator rebuilds. The world carries over: the falling cube keeps its
/// trajectory. A Speed typed or modulated past 4 lands on 4, so the world
/// keeps stepping instead of holding.
#[test]
fn adding_a_light_keeps_physics_running_and_speed_clamps() {
    let mut doc = SceneDoc::base();
    let ItemFields::Object(cube) = &mut doc.find_mut(&ItemId::new("cube")).unwrap().fields else { panic!("cube") };
    cube.physics = PhysicsSetting::Body;
    let layer = scene_layer(doc.clone());
    let fx = layer.gen_params().unwrap();
    let mut reference = runtime(fx);
    let mut edited = runtime(fx);
    for time in [0.0, 0.033, 0.12, 0.24] {
        assert_eq!(frame(&mut reference, time), frame(&mut edited, time));
    }
    let paused = frame(&mut edited, 0.24);
    assert!(paused.pos[1] < 0.0, "the cube must be falling");

    doc.insert(None, 0, Item::new(ItemId::new("lamp"), "Lamp", ItemFields::default_for(ItemKind::Light))).unwrap();
    let lit = scene_layer(doc);
    let lit = lit.gen_params().unwrap();
    assert_ne!(lit.graph, fx.graph, "the light is in the graph");
    let mut rebuilt = runtime(lit);
    rebuilt.carry_generator_state_from(&mut edited);
    assert_eq!(frame(&mut rebuilt, 0.24), paused, "adding a light reset or stepped the cube");
    for time in [0.273, 0.35] {
        assert_eq!(frame(&mut reference, time), frame(&mut rebuilt, time), "trajectory changed at {time}");
    }

    let mut fast = lit.clone();
    let speed = fast.params.get_mut("world.speed").expect("the world has a Speed");
    assert_eq!(speed.spec.clamp_input(9.0), 4.0, "a typed Speed stops at 4");
    assert_eq!(speed.spec.clamp_input(-1.0), 0.0, "and at 0");
    speed.value = speed.driven_value(9.0);
    assert_eq!(speed.value, 4.0, "a modulated Speed stops at 4");
    rebuilt.apply_param_values(&fast.params);
    let before = frame(&mut rebuilt, 0.4);
    let after = frame(&mut rebuilt, 0.5);
    assert!(after.pos[1] < before.pos[1], "the world froze at Speed 4: {before:?} then {after:?}");
}
