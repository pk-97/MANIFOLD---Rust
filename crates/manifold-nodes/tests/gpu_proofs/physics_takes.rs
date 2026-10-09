//! Bounded native-Metal proof for effect-chain fluid source provenance.
//!
//! The two cards deliberately carry the same local `fluid` and `observe` node
//! ids.  A chain build must resolve each source through its owning effect slot
//! rather than through the shared graph's first matching node.

use std::borrow::Cow;
use std::cell::Cell;
use std::sync::Arc;

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effects::PresetInstance;
use manifold_core::id::EffectId;
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::water::fluid::{FluidDomainSnapshot, FluidDomainState};
use manifold_node_engine::water::physics::PhysicsStepScope;
use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use manifold_node_engine::{exec::effect_node::EffectNode, exec::effect_node::EffectNodeContext, exec::effect_node::EffectNodeType, parameters::ParamDef, persistence::PrimitiveRegistry};
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::{ChainBuildInputs, PresetRuntime};
use manifold_node_engine::water::runtime::WaterRuntimeExt;
use manifold_node_engine::gpu::render_target::RenderTarget;


const WIDTH: u32 = 16;
const HEIGHT: u32 = 16;
const GRAVITY: &str = "gravity";
const FLUID: &str = "fluid";

thread_local! {
    static OBSERVED_TIME: Cell<Option<f32>> = const { Cell::new(None) };
}

struct ScalarLivenessObserver(EffectNodeType);

impl EffectNode for ScalarLivenessObserver {
    fn is_liveness_root(&self) -> bool {
        true
    }

    fn type_id(&self) -> &EffectNodeType {
        &self.0
    }

    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }

    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 1] = [NodePort {
            name: Cow::Borrowed("time"),
            ty: PortType::Scalar(ScalarType::F32),
            kind: PortKind::Input,
            required: true,
        }];
        &INPUTS
    }

    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }

    fn parameters(&self) -> &[ParamDef] {
        &[]
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        OBSERVED_TIME.set(
            ctx.inputs
                .scalar("time")
                .and_then(|value| value.as_scalar()),
        );
    }
}

fn instance(id: &str, cache_path: &std::path::Path, gravity: f32) -> PresetInstance {
    let mut value: serde_json::Value =
        serde_json::from_str(include_str!("../../assets/effect-presets/Invert.json"))
            .expect("canonical Invert graph parses");

    let fluid = serde_json::json!({
        "id": 3,
        "nodeId": FLUID,
        "typeId": manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID,
        "handle": FLUID,
        "params": {
            "resolution": {"type": "Int", "value": 8},
            "fill_height": {"type": "Float", "value": 0.0},
            "emission": {"type": "Float", "value": 0.0},
            "gravity_x": {"type": "Float", "value": 0.0},
            "gravity": {"type": "Float", "value": gravity},
            "gravity_z": {"type": "Float", "value": 0.0},
            "cache_mode": {"type": "Enum", "value": 1},
            "cache_path": {"type": "String", "value": cache_path.to_string_lossy()}
        }
    });
    let observer = serde_json::json!({
        "id": 4,
        "nodeId": "observe",
        "typeId": "test.physics_take_liveness",
        "handle": "observe"
    });
    value["nodes"]
        .as_array_mut()
        .expect("Invert nodes")
        .extend([fluid, observer]);
    value["wires"].as_array_mut().expect("Invert wires").push(
        serde_json::json!({"fromNode": 3, "fromPort": "simulation_time", "toNode": 4, "toPort": "time"}),
    );

    let metadata = value["presetMetadata"]
        .as_object_mut()
        .expect("Invert metadata");
    metadata["params"]
        .as_array_mut()
        .expect("Invert metadata params")
        .push(serde_json::json!({
            "id": GRAVITY,
            "name": "Gravity",
            "min": -20.0,
            "max": 20.0,
            "defaultValue": gravity
        }));
    metadata["bindings"]
        .as_array_mut()
        .expect("Invert metadata bindings")
        .push(serde_json::json!({
            "id": GRAVITY,
            "label": "Gravity",
            "defaultValue": gravity,
            "userAdded": true,
            "target": {"kind": "node", "nodeId": FLUID, "param": GRAVITY},
            "convert": {"type": "Float"}
        }));

    let graph: EffectGraphDef = serde_json::from_value(value).expect("physics take graph parses");
    let mut effect =
        manifold_core::preset_definition_registry::create_default(&PresetTypeId::INVERT_COLORS);
    effect.id = EffectId::new(id);
    effect.graph = Some(graph.clone());
    effect.reseed_param_values_from_def(&graph);
    assert!(effect.set_base_param(GRAVITY, gravity));
    effect.graph_version = 1;
    effect.graph_structure_version = 1;
    effect
}

fn set_cache_mode(effect: &mut PresetInstance, mode: u32) {
    for node in effect
        .graph
        .as_mut()
        .expect("physics take graph")
        .nodes
        .iter_mut()
    {
        if node.node_id.as_str() == FLUID {
            node.params.insert(
                "cache_mode".into(),
                manifold_core::effect_graph_def::SerializedParamValue::Enum { value: mode },
            );
        }
    }
}

fn context(time: f64) -> PresetContext {
    PresetContext {
        time,
        beat: time,
        dt: 0.1,
        width: WIDTH,
        height: HEIGHT,
        output_width: WIDTH,
        output_height: HEIGHT,
        aspect: 1.0,
        owner_key: 0xF11,
        is_clip_level: false,
        frame_count: (time * 10.0) as i64,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

fn run(
    runtime: &mut PresetRuntime,
    effects: &[PresetInstance],
    input: &RenderTarget,
    device: &Arc<manifold_gpu::GpuDevice>,
    time: f64,
) {
    let mut encoder = device.create_encoder("physics-takes-proof");
    {
        let mut gpu = GpuEncoder::new(&mut encoder, device);
        runtime
            .run(&mut gpu, &input.texture, effects, &[], &context(time))
            .expect("effect chain has a final output");
    }
    encoder.commit_and_wait_completed();
}

fn snapshot(runtime: &PresetRuntime, effect_id: &EffectId) -> FluidDomainSnapshot {
    let mut domains = Vec::new();
    runtime.water_ref().write_fluid_domains(effect_id, &mut domains);
    assert_eq!(
        domains.len(),
        1,
        "{effect_id} must expose exactly one fluid domain"
    );
    assert_eq!(domains[0].0.as_str(), FLUID);
    domains[0].1
}

fn assert_ready(runtime: &PresetRuntime, effect_id: &EffectId) -> FluidDomainSnapshot {
    let result = snapshot(runtime, effect_id);
    assert_eq!(
        result.state,
        FluidDomainState::Ready,
        "{effect_id} snapshot: {result:?}"
    );
    result
}

#[test]
fn effect_chain_fluid_takes_are_scoped_by_card_and_survive_round_trip() {
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let device = Arc::clone(&harness.device);
    let registry = {
        let mut registry = PrimitiveRegistry::with_cpu_flip_reference();
        registry.register("test.physics_take_liveness", || {
            Box::new(ScalarLivenessObserver(EffectNodeType::new(
                "test.physics_take_liveness",
            )))
        });
        registry
    };
    let suffix = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    );
    let first_path = std::env::temp_dir().join(format!("manifold-physics-take-{suffix}-first"));
    let second_path = std::env::temp_dir().join(format!("manifold-physics-take-{suffix}-second"));
    let effects = vec![
        instance("physics-take-first", &first_path, -9.81),
        instance("physics-take-second", &second_path, -4.0),
    ];
    let mut runtime = PresetRuntime::try_build(
        ChainBuildInputs {
            effects: &effects,
            groups: &[],
            primitives: &registry,
            device: &device,
            pool: None,
            width: WIDTH,
            height: HEIGHT,
            preview_effect: None,
        },
        None,
    )
    .expect("two-card physics chain builds");

    let input = RenderTarget::new(
        &device,
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        "physics-takes-input",
    );
    {
        let mut encoder = device.create_encoder("physics-takes-clear-input");
        encoder.clear_texture(&input.texture, 0.0, 0.0, 0.0, 1.0);
        encoder.commit_and_wait_completed();
    }

    let first_id = effects[0].id.clone();
    let second_id = effects[1].id.clone();
    {
        let _offline = PhysicsStepScope::for_render(true);
        run(&mut runtime, &effects, &input, &device, 0.0);
        OBSERVED_TIME.with(|time| assert_eq!(time.get(), Some(0.0)));
        run(&mut runtime, &effects, &input, &device, 0.1);
        OBSERVED_TIME.with(|time| assert_eq!(time.get(), Some(0.1)));
        assert_ready(&runtime, &first_id);
        assert_ready(&runtime, &second_id);
    }

    let serialized = serde_json::to_string(&effects).expect("physics cards serialize");
    drop(runtime);
    let effects: Vec<PresetInstance> =
        serde_json::from_str(&serialized).expect("physics cards reload");
    let mut project = manifold_core::project::Project::default();
    project.settings.master_effects = effects;
    assert_eq!(
        project.reconcile_param_manifests(),
        0,
        "reload resolves all templates"
    );
    let mut effects = project.settings.master_effects;
    set_cache_mode(&mut effects[0], 2);
    set_cache_mode(&mut effects[1], 2);
    let mut runtime = PresetRuntime::try_build(
        ChainBuildInputs {
            effects: &effects,
            groups: &[],
            primitives: &registry,
            device: &device,
            pool: None,
            width: WIDTH,
            height: HEIGHT,
            preview_effect: None,
        },
        None,
    )
    .expect("reloaded two-card physics chain builds");
    {
        let _offline = PhysicsStepScope::for_render(true);
        run(&mut runtime, &effects, &input, &device, 0.1);
        assert_ready(&runtime, &effects[0].id);
        assert_ready(&runtime, &effects[1].id);
    }

    let playback_epochs = [
        snapshot(&runtime, &effects[0].id).epoch,
        snapshot(&runtime, &effects[1].id).epoch,
    ];
    effects[0].set_param(GRAVITY, -1.0);
    effects[0].set_base_param("amount", 0.25);
    {
        let _offline = PhysicsStepScope::for_render(true);
        run(&mut runtime, &effects, &input, &device, 0.1);
    }
    assert_eq!(snapshot(&runtime, &effects[0].id).epoch, playback_epochs[0]);
    assert_eq!(snapshot(&runtime, &effects[1].id).epoch, playback_epochs[1]);
    assert_ready(&runtime, &effects[0].id);
    assert_ready(&runtime, &effects[1].id);

    effects[0].set_base_param(GRAVITY, -3.0);
    {
        let _offline = PhysicsStepScope::for_render(true);
        run(&mut runtime, &effects, &input, &device, 0.1);
    }
    assert_eq!(
        snapshot(&runtime, &effects[0].id).state,
        FluidDomainState::Failed
    );
    assert_ready(&runtime, &effects[1].id);
    assert_eq!(snapshot(&runtime, &effects[1].id).epoch, playback_epochs[1]);

    effects[0].set_base_param(GRAVITY, -9.81);
    {
        let _offline = PhysicsStepScope::for_render(true);
        run(&mut runtime, &effects, &input, &device, 0.1);
    }
    assert_ready(&runtime, &effects[0].id);

    let first_graph = effects[0].graph.as_mut().expect("first card graph");
    let fluid = first_graph
        .nodes
        .iter_mut()
        .find(|node| node.node_id.as_str() == FLUID)
        .expect("first card fluid node");
    fluid.params.insert(
        "gravity_x".into(),
        manifold_core::effect_graph_def::SerializedParamValue::Float { value: 1.5 },
    );
    effects[0].bump_graph_version();
    {
        let _offline = PhysicsStepScope::for_render(true);
        run(&mut runtime, &effects, &input, &device, 0.1);
    }
    assert_eq!(
        snapshot(&runtime, &effects[0].id).state,
        FluidDomainState::Failed
    );
    assert_ready(&runtime, &effects[1].id);

    let first_graph = effects[0].graph.as_mut().expect("first card graph");
    let fluid = first_graph
        .nodes
        .iter_mut()
        .find(|node| node.node_id.as_str() == FLUID)
        .expect("first card fluid node");
    fluid.params.insert(
        "gravity_x".into(),
        manifold_core::effect_graph_def::SerializedParamValue::Float { value: 0.0 },
    );
    effects[0].bump_graph_version();
    {
        let _offline = PhysicsStepScope::for_render(true);
        run(&mut runtime, &effects, &input, &device, 0.1);
    }
    assert_ready(&runtime, &effects[0].id);
    assert_ready(&runtime, &effects[1].id);

    let rebuilt = PresetRuntime::try_build(
        ChainBuildInputs {
            effects: &effects,
            groups: &[],
            primitives: &registry,
            device: &device,
            pool: None,
            width: WIDTH,
            height: HEIGHT,
            preview_effect: None,
        },
        Some(&mut runtime),
    )
    .expect("compatible two-card rebuild succeeds");
    runtime = rebuilt;
    {
        let _offline = PhysicsStepScope::for_render(true);
        run(&mut runtime, &effects, &input, &device, 0.1);
    }
    assert_ready(&runtime, &effects[0].id);
    assert_ready(&runtime, &effects[1].id);
    assert_eq!(
        snapshot(&runtime, &effects[1].id).epoch,
        playback_epochs[1],
        "second card keeps its source identity across the chain rebuild"
    );

    // The build has already observed the new controls before harvesting the
    // old native node. Reinstalling the current source identity after harvest
    // is essential: the next frame's unchanged host digest cannot do it again.
    effects[0].set_base_param(GRAVITY, -6.0);
    runtime = PresetRuntime::try_build(
        ChainBuildInputs {
            effects: &effects,
            groups: &[],
            primitives: &registry,
            device: &device,
            pool: None,
            width: WIDTH,
            height: HEIGHT,
            preview_effect: None,
        },
        Some(&mut runtime),
    )
    .expect("rebuild with an edited control succeeds");
    {
        let _offline = PhysicsStepScope::for_render(true);
        run(&mut runtime, &effects, &input, &device, 0.1);
    }
    assert_eq!(
        snapshot(&runtime, &effects[0].id).state,
        FluidDomainState::Failed
    );
    assert_ready(&runtime, &effects[1].id);
    assert_eq!(snapshot(&runtime, &effects[1].id).epoch, playback_epochs[1]);
    drop(runtime);
    drop(input);
    std::fs::remove_dir_all(&first_path).expect("remove first proof take");
    std::fs::remove_dir_all(&second_path).expect("remove second proof take");
}
