//! Bounded GPU proof for the CPU FLIP Water Basin reference scene.
//!
//! The proof runs the real preset runtime against native Metal at a modest
//! 640×360 target. Export/offline stepping is deliberately used so every
//! authored tick is drained before the frame is committed.

use std::sync::Arc;
use std::time::Instant;
use std::cell::Cell;

use half::f16;
use manifold_core::params::ParamManifest;
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::runtime::frame_status::{FrameRenderFailure, FrameRenderStatus};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::gpu::headless_readback::{readback_raw_halves, readback_to_srgb_png};
use manifold_node_engine::{exec::effect_node::EffectNode, exec::effect_node::EffectNodeContext, exec::effect_node::EffectNodeType, ports::NodeInput, ports::NodeOutput, ports::NodePort, parameters::ParamDef, parameters::ParamValue, ports::PortKind, ports::PortType, persistence::PrimitiveRegistry, water::physics::PhysicsStepScope};
use manifold_node_engine::scene::depth_rule::DepthRule;
use manifold_node_engine::scene::transform::Transform;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::gpu::render_target::RenderTarget;


mod explicit_authoring;
mod authored_coupling;
mod dam_break_authoring;
mod deleted_obstacle;

const WATER_BASIN_JSON: &str = manifold_nodes::testkit::assets::TESTS_FIXTURES_CPU_FLIP_WATERBASIN_JSON;
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const LAST_FRAME: u32 = 90;

#[derive(Clone, Copy, Default)]
struct CoupledObserverSample {
    simulation_time: f32,
    pose_x: f32,
    particles: f32,
}

thread_local! {
    static COUPLED_OBSERVER_SAMPLE: Cell<CoupledObserverSample> =
        const { Cell::new(CoupledObserverSample { simulation_time: 0.0, pose_x: 0.0, particles: 0.0 }) };
}

struct CoupledObserver {
    type_id: EffectNodeType,
}

impl EffectNode for CoupledObserver {
    fn depth_rule(&self) -> DepthRule {
        DepthRule::Terminal
    }

    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }

    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 3] = [
            NodePort {
                name: std::borrow::Cow::Borrowed("time"),
                ty: PortType::Scalar(manifold_node_engine::ports::ScalarType::F32),
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
                name: std::borrow::Cow::Borrowed("particles"),
                ty: PortType::Scalar(manifold_node_engine::ports::ScalarType::F32),
                kind: PortKind::Input,
                required: true,
            },
        ];
        &INPUTS
    }

    fn outputs(&self) -> &[NodeOutput] {
        static OUTPUTS: [NodeOutput; 1] = [NodePort {
            name: std::borrow::Cow::Borrowed("visible"),
            ty: PortType::Scalar(manifold_node_engine::ports::ScalarType::F32),
            kind: PortKind::Output,
            required: false,
        }];
        &OUTPUTS
    }

    fn parameters(&self) -> &[ParamDef] {
        &[]
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(ParamValue::Float(simulation_time)) = ctx.inputs.scalar("time") else {
            ctx.mark_outputs_pending();
            return;
        };
        let Some(Transform { pos: [pose_x, ..], .. }) = ctx.inputs.transform("pose") else {
            ctx.mark_outputs_pending();
            return;
        };
        let Some(ParamValue::Float(particles)) = ctx.inputs.scalar("particles") else {
            ctx.mark_outputs_pending();
            return;
        };
        COUPLED_OBSERVER_SAMPLE.with(|sample| {
            sample.set(CoupledObserverSample {
                simulation_time,
                pose_x,
                particles,
            });
        });
        ctx.outputs.set_scalar("visible", ParamValue::Float(1.0));
    }
}

fn context(frame: u32) -> PresetContext {
    let seconds = f64::from(frame) / 60.0;
    PresetContext {
        time: seconds,
        beat: seconds,
        dt: 1.0 / 60.0,
        width: WIDTH,
        height: HEIGHT,
        output_width: WIDTH,
        output_height: HEIGHT,
        aspect: WIDTH as f32 / HEIGHT as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: i64::from(frame),
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

fn render_frame(
    runtime: &mut PresetRuntime,
    target: &RenderTarget,
    device: &manifold_gpu::GpuDevice,
    frame: u32,
) -> Vec<u8> {
    let mut encoder = device.create_encoder("water-basin-proof");
    let status = {
        let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
        runtime.render(
            &mut gpu,
            &target.texture,
            &context(frame),
            &ParamManifest::default(),
        );
        gpu.frame_status()
    };
    assert_eq!(
        status,
        FrameRenderStatus::Complete,
        "Water Basin frame {frame} must complete without pending simulation or GPU work"
    );
    encoder.commit_and_wait_completed();
    readback_raw_halves(device, &target.texture, WIDTH, HEIGHT)
}

fn warmup_mesh_roles(
    runtime: &mut PresetRuntime,
    target: &RenderTarget,
    device: &manifold_gpu::GpuDevice,
) {
    // Async geometry preparation is pumped at unchanged transport time, as
    // export pre-roll does. Only complete frames advance simulation time.
    let wait = manifold_node_engine::testkit::gpu_harness::BackgroundWait::new("mesh role warmup");
    loop {
        let mut encoder = device.create_encoder("mesh-role-warmup");
        let status = {
            let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
            runtime.render(&mut gpu, &target.texture, &context(0), &ParamManifest::default());
            gpu.frame_status()
        };
        encoder.commit_and_wait_completed();
        assert!(!matches!(status, FrameRenderStatus::Failed(_)), "role warmup: {status:?}");
        if status == FrameRenderStatus::Complete && !runtime.warmup_pending() {
            return;
        }
        wait.hold();
    }
}

fn assert_finite_and_nonempty(bytes: &[u8], frame: u32) {
    assert_eq!(bytes.len(), (WIDTH * HEIGHT * 8) as usize);
    let mut nonempty = 0usize;
    for pixel in bytes.chunks_exact(8) {
        let channels = [
            f16::from_le_bytes([pixel[0], pixel[1]]).to_f32(),
            f16::from_le_bytes([pixel[2], pixel[3]]).to_f32(),
            f16::from_le_bytes([pixel[4], pixel[5]]).to_f32(),
            f16::from_le_bytes([pixel[6], pixel[7]]).to_f32(),
        ];
        assert!(
            channels.iter().all(|value| value.is_finite()),
            "Water Basin frame {frame} contains a non-finite pixel"
        );
        if channels[..3].iter().any(|value| *value > 0.001) {
            nonempty += 1;
        }
    }
    assert!(
        nonempty > 0,
        "Water Basin frame {frame} rendered no nonempty pixels"
    );
}

fn assert_pixels_close(before: &[u8], after: &[u8], tolerance: f32) {
    assert_eq!(before.len(), after.len());
    for (before, after) in before.chunks_exact(8).zip(after.chunks_exact(8)) {
        for channel in 0..4 {
            let offset = channel * 2;
            let before = f16::from_le_bytes([before[offset], before[offset + 1]]).to_f32();
            let after = f16::from_le_bytes([after[offset], after[offset + 1]]).to_f32();
            assert!((before - after).abs() <= tolerance, "paused frame changed: {before} vs {after}");
        }
    }
}

#[test]
fn water_basin_renders_complete_finite_frames_through_tick_90() {
    let started = Instant::now();
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let registry = PrimitiveRegistry::with_cpu_flip_reference();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        WATER_BASIN_JSON,
        &registry,
        Arc::clone(&harness.device),
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|error| panic!("Water Basin graph must build: {error}"));
    let target = RenderTarget::new(
        &harness.device,
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        "water-basin-proof",
    );

    // Offline mode makes the worker drain all due fixed 60 Hz ticks before
    // returning, which keeps this proof serial and bounded.
    let _offline = PhysicsStepScope::for_render(true);
    for frame in 0..=LAST_FRAME {
        let pixels = render_frame(&mut runtime, &target, &harness.device, frame);
        assert_finite_and_nonempty(&pixels, frame);
        if matches!(frame, 1 | 30 | 90) {
            let path = format!("/tmp/manifold_water_{frame}.png");
            std::fs::write(
                &path,
                readback_to_srgb_png(&harness.device, &target.texture, WIDTH, HEIGHT),
            )
            .unwrap_or_else(|error| panic!("write {path}: {error}"));
        }
    }
    eprintln!(
        "Water Basin GPU proof: ticks 0..={LAST_FRAME} at {WIDTH}x{HEIGHT}, elapsed={:.2?}, artifacts=/tmp/manifold_water_1.png,/tmp/manifold_water_30.png,/tmp/manifold_water_90.png",
        started.elapsed()
    );
    std::fs::write("/tmp/manifold_water_timing.txt", format!(
        "90 solver ticks plus initialization; 640x360, readback every frame, three PNG encodes. Total wall time including setup: {:.3} seconds. This is a headless proof, not app FPS.\n", started.elapsed().as_secs_f64()
    )).unwrap();
}

#[test]
fn water_basin_paired_rigid_pose_publishes_through_fluid_worker() {
    let mut def: serde_json::Value = serde_json::from_str(WATER_BASIN_JSON).unwrap();
    // Keep this fixture's authored controls authoritative: the reference
    // preset's exposed defaults otherwise re-enable Pour and resolution 24.
    def.as_object_mut().unwrap().remove("presetMetadata");
    let nodes = def["nodes"].as_array_mut().unwrap();
    let fluid = nodes.iter_mut().find(|node| node["id"] == 4).unwrap();
    for (name, value) in [
        ("resolution", 12.0),
        ("fill_height", 1.0),
        ("emission", 0.0),
        ("gravity", 0.0),
    ] {
        fluid["params"][name] = serde_json::json!({"type": "Float", "value": value});
    }
    nodes.extend([
        serde_json::json!({
            "id": 500, "nodeId": "paired_physics", "typeId": "node.physics_world",
            "handle": "Paired Physics", "params": {
                "gravity_x": {"type": "Float", "value": 0.0},
                "gravity_y": {"type": "Float", "value": 0.0},
                "gravity_z": {"type": "Float", "value": 0.0},
                "speed": {"type": "Float", "value": 1.0},
                "reset": {"type": "Float", "value": 0.0}
            }
        }),
        serde_json::json!({
            "id": 501, "nodeId": "paired_body", "typeId": "node.rigid_body",
            "handle": "Paired Moving Body", "params": {
                "enabled": {"type": "Bool", "value": true},
                "shape": {"type": "Enum", "value": 1},
                "motion": {"type": "Enum", "value": 1},
                "density": {"type": "Float", "value": 1000.0},
                "friction": {"type": "Float", "value": 0.2},
                "bounce": {"type": "Float", "value": 0.0}
            }
        }),
        serde_json::json!({
            "id": 502, "nodeId": "paired_acceleration", "typeId": "node.uniform_vector_field",
            "handle": "Paired Acceleration", "params": {
                "x": {"type": "Float", "value": 20.0},
                "y": {"type": "Float", "value": 0.0},
                "z": {"type": "Float", "value": 0.0}
            }
        }),
        serde_json::json!({
            "id": 503, "nodeId": "paired_observer", "typeId": "node.test_coupled_observer",
            "handle": "Coupled Observer"
        }),
    ]);
    let wires = def["wires"].as_array_mut().unwrap();
    wires.retain(|wire| {
        !((wire["fromNode"] == 6 && wire["toNode"] == 7)
            || (wire["fromNode"] == 7 && wire["toNode"] == 4)
            || (wire["fromNode"] == 4
                && wire["fromPort"] == "obstacle_pose"
                && wire["toNode"] == 12
                && wire["toPort"] == "transform"))
    });
    wires.extend([
        serde_json::json!({"fromNode": 7, "fromPort": "transform", "toNode": 501, "toPort": "transform"}),
        serde_json::json!({"fromNode": 501, "fromPort": "body", "toNode": 500, "toPort": "body_0"}),
        serde_json::json!({"fromNode": 502, "fromPort": "out", "toNode": 500, "toPort": "acceleration_field"}),
        serde_json::json!({"fromNode": 500, "fromPort": "pose_0", "toNode": 12, "toPort": "transform"}),
        serde_json::json!({"fromNode": 4, "fromPort": "simulation_time", "toNode": 503, "toPort": "time"}),
        serde_json::json!({"fromNode": 4, "fromPort": "particle_count", "toNode": 503, "toPort": "particles"}),
        serde_json::json!({"fromNode": 500, "fromPort": "pose_0", "toNode": 503, "toPort": "pose"}),
        serde_json::json!({"fromNode": 503, "fromPort": "visible", "toNode": 12, "toPort": "visible"}),
    ]);

    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let mut registry = PrimitiveRegistry::with_cpu_flip_reference();
    registry.register("node.test_coupled_observer", || {
        Box::new(CoupledObserver {
            type_id: EffectNodeType::new("node.test_coupled_observer"),
        })
    });
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &def.to_string(),
        &registry,
        Arc::clone(&harness.device),
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|error| panic!("paired Water Basin graph must build: {error}"));
    let target = RenderTarget::new(
        &harness.device,
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        "paired-water-basin-proof",
    );
    let _offline = PhysicsStepScope::for_render(true);
    let initial = render_frame(&mut runtime, &target, &harness.device, 0);
    assert_finite_and_nonempty(&initial, 0);
    let initial_observer = COUPLED_OBSERVER_SAMPLE.with(Cell::get);
    assert!(initial_observer.simulation_time.abs() < 1.0e-6);
    let progressed = (1..=3).fold(initial.clone(), |_, frame| {
        let pixels = render_frame(&mut runtime, &target, &harness.device, frame);
        assert_finite_and_nonempty(&pixels, frame);
        pixels
    });
    let progressed_observer = COUPLED_OBSERVER_SAMPLE.with(Cell::get);
    assert!((progressed_observer.simulation_time - 3.0 / 60.0).abs() < 1.0e-6);
    assert!(progressed_observer.particles > 0.0, "paired fixture must contain liquid");
    assert!(
        progressed_observer.pose_x > initial_observer.pose_x + 1.0e-4,
        "coupled rigid pose must advance: {} -> {}",
        initial_observer.pose_x,
        progressed_observer.pose_x
    );
    let changed = initial
        .chunks_exact(8)
        .zip(progressed.chunks_exact(8))
        .filter(|(before, after)| {
            (0..3).any(|channel| {
                let offset = channel * 2;
                let before = f16::from_le_bytes([before[offset], before[offset + 1]]).to_f32();
                let after = f16::from_le_bytes([after[offset], after[offset + 1]]).to_f32();
                (before - after).abs() > 0.001
            })
        })
        .count();
    assert!(changed > 0, "paired rigid pose must move the rendered obstacle");
    let paused = render_frame(&mut runtime, &target, &harness.device, 3);
    assert_finite_and_nonempty(&paused, 3);
    assert_pixels_close(&progressed, &paused, 1.0e-3);
    let paused_observer = COUPLED_OBSERVER_SAMPLE.with(Cell::get);
    assert!((paused_observer.simulation_time - progressed_observer.simulation_time).abs() < 1.0e-6);
    assert!((paused_observer.pose_x - progressed_observer.pose_x).abs() < 1.0e-6);
    std::fs::write(
        "/tmp/manifold_coupled_scene.png",
        readback_to_srgb_png(&harness.device, &target.texture, WIDTH, HEIGHT),
    )
    .expect("write coupled scene proof image");
}

#[test]
fn water_invalid_configuration_marks_frame_failed_for_export() {
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let mut def: serde_json::Value = serde_json::from_str(WATER_BASIN_JSON).unwrap();
    let fluid = def["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|node| node["nodeId"] == "fluid_surface")
        .unwrap();
    fluid["params"]["fill_height"]["value"] = serde_json::json!(-1.0);
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &serde_json::to_string(&def).unwrap(),
        &PrimitiveRegistry::with_cpu_flip_reference(),
        Arc::clone(&harness.device),
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap();
    let target = RenderTarget::new(
        &harness.device,
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        "invalid-water",
    );
    let _offline = PhysicsStepScope::for_render(true);
    let mut encoder = harness.device.create_encoder("invalid-water");
    {
        let mut gpu = RendererGpuEncoder::new(&mut encoder, &harness.device);
        runtime.render(
            &mut gpu,
            &target.texture,
            &context(0),
            &ParamManifest::default(),
        );
        assert_eq!(
            gpu.frame_status(),
            FrameRenderStatus::Failed(FrameRenderFailure::Simulation)
        );
    }
    encoder.commit_and_wait_completed();
}

#[test]
fn scene_physics_added_fluid_renders_after_project_reload() {
    use manifold_core::{GraphTarget, PresetTypeId, layer::Layer, project::Project};
    use manifold_editing::command::Command;
    use manifold_editing::commands::graph::AddSceneFluidCommand;
    use {manifold_nodes::bundled_presets::bundled_preset_def, manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type};

    let mut project = Project::default();
    let preset = PresetTypeId::new("Scene");
    let baseline = bundled_preset_def(&preset).unwrap();
    let render_id = baseline.nodes.iter().find(|node| node.type_id == "node.render_scene").unwrap().id;
    let layer = Layer::new_generator("Fluid Authoring".into(), preset, 0);
    let target_graph = GraphTarget::Generator(layer.layer_id.clone());
    project.timeline.layers.push(layer);
    let mut add = AddSceneFluidCommand::new(target_graph.clone(), render_id,
        manifold_nodes::testkit::reference_fixtures::cpu_flip_metadata(), metadata_for_node_type("node.transform_3d"),
        metadata_for_node_type("node.pbr_material"), metadata_for_node_type("node.scene_object"),
        manifold_editing::commands::graph::flip_scene_fluid_template(), baseline.as_ref().clone())
        .with_world_metadata(metadata_for_node_type("node.physics_world"));
    add.execute(&mut project);
    assert!(add.was_applied(), "{:?}", add.rejection_reason());
    let saved = serde_json::to_string(&project).unwrap();
    let reloaded: Project = serde_json::from_str(&saved).unwrap();
    let graph = reloaded.graph_for_target(&target_graph, None).unwrap();
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let registry = PrimitiveRegistry::with_cpu_flip_reference();
    let build = |def| PresetRuntime::from_json_str_with_device(
        &serde_json::to_string(def).unwrap(), &registry, Arc::clone(&harness.device),
        WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, None).unwrap();
    let mut base_runtime = build(baseline.as_ref());
    let mut fluid_runtime = build(graph);
    let target = RenderTarget::new(&harness.device, WIDTH, HEIGHT,
        GpuTextureFormat::Rgba16Float, "added-fluid-proof");
    let _offline = PhysicsStepScope::for_render(true);
    let base_pixels = render_frame(&mut base_runtime, &target, &harness.device, 0);
    warmup_mesh_roles(&mut fluid_runtime, &target, &harness.device);
    let mut fluid_pixels = Vec::new();
    for frame in 0..=30 {
        fluid_pixels = render_frame(&mut fluid_runtime, &target, &harness.device, frame);
    }
    assert_finite_and_nonempty(&fluid_pixels, 30);
    let changed = base_pixels.chunks_exact(8).zip(fluid_pixels.chunks_exact(8))
        .filter(|(a, b)| (0..3).any(|axis| {
            let offset = axis * 2;
            let a = f16::from_le_bytes([a[offset], a[offset + 1]]).to_f32();
            let b = f16::from_le_bytes([b[offset], b[offset + 1]]).to_f32();
            (a - b).abs() > 0.01
        })).count();
    assert!(changed > 200, "added fluid must visibly affect the existing scene: {changed} pixels");
    std::fs::write("/tmp/manifold_added_fluid.png",
        readback_to_srgb_png(&harness.device, &target.texture, WIDTH, HEIGHT)).unwrap();
}

#[test]
fn scene_physics_invalid_mesh_role_fails_instead_of_waiting_for_preparation() {
    let mut def: serde_json::Value = serde_json::from_str(WATER_BASIN_JSON).unwrap();
    def["nodes"].as_array_mut().unwrap().push(serde_json::json!({
        "id": 500, "nodeId": "invalid_source", "typeId": "node.fluid_role_source",
        "params": { "radius": {"type": "Float", "value": -1.0} }
    }));
    def["wires"].as_array_mut().unwrap().extend([
        serde_json::json!({"fromNode": 5, "fromPort": "transform", "toNode": 500, "toPort": "transform"}),
        serde_json::json!({"fromNode": 500, "fromPort": "role", "toNode": 4, "toPort": "role_0"})
    ]);
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &def.to_string(), &PrimitiveRegistry::with_cpu_flip_reference(), Arc::clone(&harness.device),
        WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, None).unwrap();
    let target = RenderTarget::new(&harness.device, WIDTH, HEIGHT,
        GpuTextureFormat::Rgba16Float, "invalid-mesh-role");
    let _offline = PhysicsStepScope::for_render(true);
    let mut encoder = harness.device.create_encoder("invalid-mesh-role");
    let status = {
        let mut gpu = RendererGpuEncoder::new(&mut encoder, &harness.device);
        runtime.render(&mut gpu, &target.texture, &context(0), &ParamManifest::default());
        gpu.frame_status()
    };
    encoder.commit_and_wait_completed();
    assert_eq!(status, FrameRenderStatus::Failed(FrameRenderFailure::InvalidGeometry));
}

#[test]
fn scene_physics_shared_field_changes_rendered_liquid_after_graph_round_trip() {
    let mut def: serde_json::Value = serde_json::from_str(WATER_BASIN_JSON).unwrap();
    let nodes = def["nodes"].as_array_mut().unwrap();
    let fluid = nodes.iter_mut().find(|node| node["id"] == 4).unwrap();
    for (name, value) in [("resolution", 8.0), ("fill_height", 0.0), ("emission", 0.0), ("gravity", 0.0)] {
        fluid["params"][name] = serde_json::json!({"type": "Float", "value": value});
    }
    nodes.extend([
        serde_json::json!({"id": 500, "nodeId": "seed", "typeId": "node.transform_3d", "params": {
            "pos_y": {"type":"Float","value":1.5},
            "scale_x": {"type":"Float","value":1.5},
            "scale_y": {"type":"Float","value":1.5},
            "scale_z": {"type":"Float","value":1.5}
        }}),
        serde_json::json!({"id": 501, "nodeId": "direction", "typeId": "node.uniform_vector_field", "params": {
            "x": {"type":"Float","value":1.0}, "y": {"type":"Float","value":0.0}
        }}),
        serde_json::json!({"id": 502, "nodeId": "strength", "typeId": "node.scale_vector_field", "params": {
            "strength": {"type":"Float","value":8.0}
        }})
    ]);
    let wires = def["wires"].as_array_mut().unwrap();
    wires.retain(|wire| wire["toNode"] != 4);
    wires.extend([
        serde_json::json!({"fromNode":500,"fromPort":"transform","toNode":4,"toPort":"initial_volume"}),
        serde_json::json!({"fromNode":501,"fromPort":"out","toNode":502,"toPort":"field"}),
        serde_json::json!({"fromNode":502,"fromPort":"out","toNode":4,"toPort":"acceleration_field"})
    ]);
    let typed: manifold_core::effect_graph_def::EffectGraphDef = serde_json::from_value(def.clone()).unwrap();
    let saved = serde_json::to_string(&typed).unwrap();
    let restored: manifold_core::effect_graph_def::EffectGraphDef = serde_json::from_str(&saved).unwrap();
    assert_eq!(typed, restored);
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let registry = PrimitiveRegistry::with_cpu_flip_reference();
    let build = |json: &str| PresetRuntime::from_json_str_with_device(
        json, &registry, Arc::clone(&harness.device), WIDTH, HEIGHT,
        GpuTextureFormat::Rgba16Float, None).unwrap();
    let mut forced = build(&saved);
    def["nodes"].as_array_mut().unwrap().iter_mut().find(|node| node["id"] == 502).unwrap()
        ["params"]["strength"]["value"] = serde_json::json!(0.0);
    let mut resting = build(&def.to_string());
    let target = RenderTarget::new(&harness.device, WIDTH, HEIGHT,
        GpuTextureFormat::Rgba16Float, "shared-field-liquid");
    let _offline = PhysicsStepScope::for_render(true);
    for runtime in [&mut forced, &mut resting] {
        render_frame(runtime, &target, &harness.device, 0);
    }
    let before = render_frame(&mut resting, &target, &harness.device, 8);
    let after = render_frame(&mut forced, &target, &harness.device, 8);
    assert_finite_and_nonempty(&after, 8);
    let changed = before.chunks_exact(8).zip(after.chunks_exact(8)).filter(|(a, b)| {
        (0..3).any(|axis| {
            let i = axis * 2;
            (f16::from_le_bytes([a[i], a[i + 1]]).to_f32()
                - f16::from_le_bytes([b[i], b[i + 1]]).to_f32()).abs() > 0.01
        })
    }).count();
    assert!(changed > 50, "shared field must change the rendered fluid: {changed} pixels");
}

#[test]
fn scene_physics_mesh_role_renders_after_graph_round_trip() {
    let mut def: serde_json::Value = serde_json::from_str(WATER_BASIN_JSON).unwrap();
    let nodes = def["nodes"].as_array_mut().unwrap();
    let fluid = nodes.iter_mut().find(|node| node["id"] == 4).unwrap();
    for (name, value) in [("resolution", 12.0), ("fill_height", 0.0), ("emission", 0.0), ("gravity", 0.0)] {
        fluid["params"][name] = serde_json::json!({"type": "Float", "value": value});
    }
    nodes.extend([
        serde_json::json!({"id": 500, "nodeId": "mesh_fill_pose", "typeId": "node.transform_3d", "params": {
            "pos_y": {"type": "Float", "value": 1.3},
            "rot_y": {"type": "Float", "value": 0.7}
        }}),
        serde_json::json!({"id": 501, "nodeId": "mesh_fill", "typeId": "node.fluid_role_source", "params": {
            "role": {"type": "Enum", "value": 0},
            "shape": {"type": "Enum", "value": 0},
            "radius": {"type": "Float", "value": 1.2},
            "enabled": {"type": "Bool", "value": true}
        }})
    ]);
    let wires = def["wires"].as_array_mut().unwrap();
    wires.retain(|wire| wire["toNode"] != 4);
    wires.extend([
        serde_json::json!({"fromNode": 500, "fromPort": "transform", "toNode": 501, "toPort": "transform"}),
        serde_json::json!({"fromNode": 501, "fromPort": "role", "toNode": 4, "toPort": "role_0"})
    ]);
    let typed: manifold_core::effect_graph_def::EffectGraphDef = serde_json::from_value(def.clone()).unwrap();
    let saved = serde_json::to_string(&typed).unwrap();
    let restored: manifold_core::effect_graph_def::EffectGraphDef = serde_json::from_str(&saved).unwrap();
    assert_eq!(typed, restored);
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let registry = PrimitiveRegistry::with_cpu_flip_reference();
    let build = |json: &str| PresetRuntime::from_json_str_with_device(
        json, &registry, Arc::clone(&harness.device), WIDTH, HEIGHT,
        GpuTextureFormat::Rgba16Float, None).unwrap();
    let mut fluid_runtime = build(&saved);
    def["nodes"].as_array_mut().unwrap().iter_mut().find(|n| n["id"] == 501).unwrap()
        ["params"]["enabled"]["value"] = serde_json::json!(false);
    let mut empty_runtime = build(&def.to_string());
    let target = RenderTarget::new(&harness.device, WIDTH, HEIGHT,
        GpuTextureFormat::Rgba16Float, "mesh-role-proof");
    let _offline = PhysicsStepScope::for_render(true);
    for runtime in [&mut fluid_runtime, &mut empty_runtime] {
        warmup_mesh_roles(runtime, &target, &harness.device);
    }
    let empty = render_frame(&mut empty_runtime, &target, &harness.device, 1);
    let liquid = render_frame(&mut fluid_runtime, &target, &harness.device, 1);
    assert_finite_and_nonempty(&liquid, 1);
    let changed = empty.chunks_exact(8).zip(liquid.chunks_exact(8)).filter(|(a, b)| {
        (0..3).any(|axis| {
            let i = axis * 2;
            (f16::from_le_bytes([a[i], a[i + 1]]).to_f32()
                - f16::from_le_bytes([b[i], b[i + 1]]).to_f32()).abs() > 0.01
        })
    }).count();
    assert!(changed > 100, "mesh-based fill must visibly affect the scene: {changed} pixels");
    std::fs::write("/tmp/manifold_fluid_mesh_role.png",
        readback_to_srgb_png(&harness.device, &target.texture, WIDTH, HEIGHT)).unwrap();
}

#[test]
fn scene_physics_assigned_object_fills_fluid_through_group_boundaries() {
    use manifold_core::{GraphTarget, PresetTypeId, layer::Layer, project::Project};
    use manifold_core::effect_graph_def::SerializedParamValue;
    use manifold_core::scene_modifier_preset::SceneNodeRef;
    use manifold_editing::command::Command;
    use manifold_editing::commands::graph::{AddSceneFluidCommand, AddSceneObjectCommand, AssignSceneFluidRoleCommand};
    use {manifold_nodes::bundled_presets::bundled_preset_def, manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type};
    use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectVm, SceneVm};

    let mut project = Project::default();
    let preset = PresetTypeId::new("Scene");
    let baseline = bundled_preset_def(&preset).unwrap();
    let render_id = baseline.nodes.iter().find(|node| node.type_id == "node.render_scene").unwrap().id;
    let layer = Layer::new_generator("Assigned Mesh Fill".into(), preset, 0);
    let target_graph = GraphTarget::Generator(layer.layer_id.clone());
    project.timeline.layers.push(layer);
    let mut add_fluid = AddSceneFluidCommand::new(target_graph.clone(), render_id,
        manifold_nodes::testkit::reference_fixtures::cpu_flip_metadata(), metadata_for_node_type("node.transform_3d"),
        metadata_for_node_type("node.pbr_material"), metadata_for_node_type("node.scene_object"),
        manifold_editing::commands::graph::flip_scene_fluid_template(), baseline.as_ref().clone())
        .with_world_metadata(metadata_for_node_type("node.physics_world"));
    add_fluid.execute(&mut project);
    assert!(add_fluid.was_applied());
    let mut add_object = AddSceneObjectCommand::new(target_graph.clone(), vec![], render_id, 0,
        (0.0, 0.0), vec![], vec![], vec![], baseline.as_ref().clone());
    add_object.execute(&mut project);
    assert!(add_object.was_applied());
    let def = project.graph_for_target(&target_graph, None).unwrap();
    let vm = SceneVm::from_def(def).unwrap();
    let object = vm.objects.iter().filter_map(|object| match object {
        SceneObjectVm::Known(row) if row.group_node_id.is_some() && row.fluid_controls.is_empty() => Some(row),
        _ => None,
    }).max_by_key(|row| row.index).unwrap();
    let object_index = object.index as u32;
    let object_group_id = object.group_node_id.unwrap();
    let fluid_group = def.nodes.iter().find(|node| node.group.as_ref().is_some_and(|group|
        group.nodes.iter().any(|node| node.type_id == manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID))).unwrap();
    let fluid = fluid_group.group.as_ref().unwrap().nodes.iter().find(|node| node.type_id == manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID).unwrap();
    let domain = SceneNodeRef { scope: vec![fluid_group.node_id.clone()], node: fluid.node_id.clone() };
    let mut assign = AssignSceneFluidRoleCommand::new(target_graph.clone(), render_id, object_index,
        domain, 0, vec![], baseline.as_ref().clone());
    assign.execute(&mut project);
    assert!(assign.was_applied(), "{:?}", assign.rejection_reason());
    let mut def = project.graph_for_target(&target_graph, None).unwrap().clone();
    for group in &mut def.nodes {
        let is_source = group.id == object_group_id;
        let Some(body) = &mut group.group else { continue; };
        let domain_node = body.wires.iter().find(|wire| wire.to_port == "domain").map(|wire| wire.from_node);
        for node in &mut body.nodes {
            match node.type_id.as_str() {
                manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID => {
                    for (name, value) in [("resolution", 12.0), ("fill_height", 0.0), ("emission", 0.0), ("gravity", 0.0)] {
                        node.params.insert(name.into(), SerializedParamValue::Float { value });
                    }
                }
                "node.fluid_role_source" if !is_source => {
                    node.params.insert("enabled".into(), SerializedParamValue::Bool { value: false });
                }
                "node.scene_object" if is_source => {
                    node.params.insert("visible".into(), SerializedParamValue::Float { value: 0.0 });
                }
                "node.transform_3d" if Some(node.id) == domain_node => {
                    for (name, value) in [("pos_x", -0.3), ("pos_y", 1.5), ("pos_z", 0.2),
                        ("scale_x", 5.0), ("scale_y", 3.0), ("scale_z", 2.5)] {
                        node.params.insert(name.into(), SerializedParamValue::Float { value });
                    }
                }
                "node.transform_3d" if is_source => {
                    node.params.insert("pos_y".into(), SerializedParamValue::Float { value: 1.3 });
                }
                _ => {}
            }
        }
    }
    let camera = manifold_node_engine::scene::viewport_camera::ViewportCamera {
        target: [0.0, 1.0, 0.0], ..Default::default()
    };
    let render_node = def.nodes.iter().find(|node| node.id == render_id).unwrap().node_id.clone();
    let def = manifold_nodes_scene::node_graph::viewport_render::override_camera_def(&def, &render_node, &camera).unwrap();
    let mut def = def;
    let saved = serde_json::to_string(&def).unwrap();
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let registry = PrimitiveRegistry::with_cpu_flip_reference();
    let build = |json: &str| PresetRuntime::from_json_str_with_device(json, &registry,
        Arc::clone(&harness.device), WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, None).unwrap();
    let mut fluid_runtime = build(&saved);
    let object_group = def.nodes.iter_mut().find(|node| node.id == object_group_id).unwrap().group.as_mut().unwrap();
    let role = object_group.nodes.iter_mut().find(|node| node.type_id == "node.fluid_role_source").unwrap();
    role.params.insert("enabled".into(), SerializedParamValue::Bool { value: false });
    let mut empty_runtime = build(&serde_json::to_string(&def).unwrap());
    let target = RenderTarget::new(&harness.device, WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, "assigned-fluid-role");
    let _offline = PhysicsStepScope::for_render(true);
    for runtime in [&mut fluid_runtime, &mut empty_runtime] {
        warmup_mesh_roles(runtime, &target, &harness.device);
    }
    let empty = render_frame(&mut empty_runtime, &target, &harness.device, 1);
    let liquid = render_frame(&mut fluid_runtime, &target, &harness.device, 1);
    assert_finite_and_nonempty(&liquid, 1);
    let changed = empty.chunks_exact(8).zip(liquid.chunks_exact(8)).filter(|(a, b)| {
        (0..3).any(|axis| {
            let i = axis * 2;
            (f16::from_le_bytes([a[i], a[i + 1]]).to_f32()
                - f16::from_le_bytes([b[i], b[i + 1]]).to_f32()).abs() > 0.01
        })
    }).count();
    assert!(changed > 100, "assigned fill must produce visible liquid: {changed} pixels");

    // Edit the original visible source after assignment, using the normal
    // in-place parameter update. Only liquid can change this image because
    // the source object is hidden. No role selector is copied or edited.
    let mut edited: manifold_core::effect_graph_def::EffectGraphDef = serde_json::from_str(&saved).unwrap();
    let group = edited.nodes.iter_mut().find(|node| node.id == object_group_id).unwrap().group.as_mut().unwrap();
    let cube = group.nodes.iter_mut().find(|node| node.type_id == "node.cube_mesh").unwrap();
    cube.params.insert("size".into(), SerializedParamValue::Float { value: 1.6 });
    fluid_runtime.apply_inner_param_overrides(&edited);
    warmup_mesh_roles(&mut fluid_runtime, &target, &harness.device);
    let resized = render_frame(&mut fluid_runtime, &target, &harness.device, 1);
    assert_finite_and_nonempty(&resized, 1);
    let changed_size = liquid.chunks_exact(8).zip(resized.chunks_exact(8)).filter(|(a, b)| {
        (0..3).any(|axis| {
            let i = axis * 2;
            (f16::from_le_bytes([a[i], a[i + 1]]).to_f32()
                - f16::from_le_bytes([b[i], b[i + 1]]).to_f32()).abs() > 0.01
        })
    }).count();
    assert!(changed_size > 100, "visible mesh size edit must change assigned liquid geometry: {changed_size} pixels");
    let scene = SceneVm::from_def(&edited).unwrap();
    let domain = scene.objects.iter().find_map(|row| match row {
        SceneObjectVm::Known(row) if !row.fluid_controls.is_empty() => row.fluid_domain,
        _ => None,
    }).expect("assigned domain bounds survive save/reload and mesh edit");
    assert_eq!(domain.size, [5.0, 3.125, 2.5]);
    let lines = manifold_nodes_scene::node_graph::viewport_overlay::fluid_domain_lines(domain);
    let projected = manifold_nodes_scene::node_graph::viewport_overlay::project_lines(&camera.to_camera(), WIDTH, HEIGHT, &lines);
    assert_eq!(projected.len(), 12, "container entirely in the editor view");
    let mut pixels = manifold_node_engine::gpu::headless_readback::readback_tonemapped_rgba8(&harness.device, &target.texture, WIDTH, HEIGHT);
    let clean = pixels.clone();
    manifold_nodes_scene::node_graph::viewport_overlay::composite_overlay_lines_rgba8(&mut pixels, WIDTH, HEIGHT, &projected);
    assert!(clean.chunks_exact(4).zip(pixels.chunks_exact(4)).filter(|(a,b)| a != b).count() > 100,
        "domain must be visibly outlined");
    std::fs::write("/tmp/manifold_assigned_fluid.png",
        manifold_node_engine::gpu::headless_readback::encode_rgba8_png(&pixels, WIDTH, HEIGHT)).unwrap();
}

#[test]
fn scene_physics_modifier_impulse_changes_rendered_liquid() {
    use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
    use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
    use manifold_core::{Beats, NodeId, Seconds};
    use manifold_node_engine::exec::effect_node::FrameTime;
    use manifold_physics::VectorField;

    let mut raw: serde_json::Value = serde_json::from_str(WATER_BASIN_JSON).unwrap();
    let nodes = raw["nodes"].as_array_mut().unwrap();
    let fluid = nodes.iter_mut().find(|node| node["id"] == 4).unwrap();
    for (name, value) in [("resolution", 8.0), ("fill_height", 0.0), ("emission", 0.0), ("gravity", 0.0)] {
        fluid["params"][name] = serde_json::json!({"type":"Float","value":value});
    }
    nodes.push(serde_json::json!({"id":500,"nodeId":"seed","typeId":"node.transform_3d","params":{
        "pos_y":{"type":"Float","value":1.5},
        "scale_x":{"type":"Float","value":1.5},
        "scale_y":{"type":"Float","value":1.5},
        "scale_z":{"type":"Float","value":1.5}
    }}));
    let wires = raw["wires"].as_array_mut().unwrap();
    wires.retain(|wire| wire["toNode"] != 4);
    wires.push(serde_json::json!({"fromNode":500,"fromPort":"transform","toNode":4,"toPort":"initial_volume"}));
    let owner: EffectGraphDef = serde_json::from_value(raw).unwrap();
    let mut recipe: EffectGraphDef = serde_json::from_str(manifold_nodes::testkit::assets::ASSETS_SCENE_MODIFIER_PRESETS_UNIFORMFORCE_JSON).unwrap();
    let metadata = recipe.preset_metadata.as_mut().unwrap();
    for (id, value) in [("strength", 0.0), ("impulse_strength", 3.0), ("direction_x", 1.0), ("direction_y", 0.0)] {
        metadata.params.iter_mut().find(|param| param.id == id).unwrap().default_value = value;
        metadata.bindings.iter_mut().find(|binding| binding.id == id).unwrap().default_value = value;
    }
    let instance = manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
        &owner, &recipe, NodeId::new("impulse"), SceneNodeRef { scope: vec![], node: NodeId::new("scene") },
        SceneTargetSelection::Explicit { objects: vec![SceneNodeRef { scope: vec![], node: NodeId::new("water_object") }] },
    ).unwrap();
    let def = manifold_core::scene_modifier_edit::insert_scene_modifier(&owner, 0, instance).unwrap().graph;
    let fire = def.preset_metadata.as_ref().unwrap().bindings.iter().find(|binding|
        matches!(&binding.target, BindingTarget::SceneModifier { param_id, .. } if param_id == "fire")
    ).unwrap().id.clone();
    let saved = serde_json::to_string(&def).unwrap();
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let build = || PresetRuntime::from_json_str_with_device(&saved, &PrimitiveRegistry::with_cpu_flip_reference(),
        Arc::clone(&harness.device), WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, None).unwrap();
    let mut resting = build();
    let mut hit = build();
    let target = RenderTarget::new(&harness.device, WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, "modifier-impulse-liquid");
    let _offline = PhysicsStepScope::for_render(true);
    render_frame(&mut resting, &target, &harness.device, 0);
    render_frame(&mut hit, &target, &harness.device, 0);
    // Initial-volume particles enter FLIP on its first native step. Fire
    // after that step so this proof measures an impulse on existing liquid.
    render_frame(&mut resting, &target, &harness.device, 1);
    render_frame(&mut hit, &target, &harness.device, 1);
    hit.fire_scene_impulse(&fire, FrameTime { seconds: Seconds(1.0 / 60.0), beats: Beats(1.0 / 60.0),
        delta: Seconds::ZERO, frame_count: 1 }, &mut 0).unwrap();
    let before = render_frame(&mut resting, &target, &harness.device, 8);
    std::fs::write("/tmp/scene_impulse_resting.png", readback_to_srgb_png(&harness.device, &target.texture, WIDTH, HEIGHT)).unwrap();
    let after = render_frame(&mut hit, &target, &harness.device, 8);
    std::fs::write("/tmp/scene_impulse_fired.png", readback_to_srgb_png(&harness.device, &target.texture, WIDTH, HEIGHT)).unwrap();
    assert_finite_and_nonempty(&after, 8);
    let mut receipts = 0;
    hit.drain_scene_impulses(|id, event| {
        assert_eq!(id.as_str(), "fluid_surface");
        assert_eq!(event.value.field.sample([0.0; 3]), [3.0, 0.0, 0.0]);
        receipts += 1;
    });
    assert_eq!(receipts, 1);
    assert!(manifold_node_engine::gpu::headless_readback::mean_abs_half_diff(&before, &after) > 0.0001,
        "a fired field must visibly change the liquid");
}
