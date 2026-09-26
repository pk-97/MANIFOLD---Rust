//! Bounded GPU proof for the CPU FLIP Water Basin reference scene.
//!
//! The proof runs the real preset runtime against native Metal at a modest
//! 640×360 target. Export/offline stepping is deliberately used so every
//! authored tick is drained before the frame is committed.

use std::sync::Arc;
use std::time::Instant;

use half::f16;
use manifold_core::params::ParamManifest;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::frame_status::{FrameRenderFailure, FrameRenderStatus};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::headless_readback::{readback_raw_halves, readback_to_srgb_png};
use manifold_renderer::node_graph::{PrimitiveRegistry, physics::PhysicsStepScope};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

use crate::harness;

const WATER_BASIN_JSON: &str = include_str!("../../assets/generator-presets/WaterBasin.json");
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const LAST_FRAME: u32 = 90;

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

#[test]
fn water_basin_renders_complete_finite_frames_through_tick_90() {
    let started = Instant::now();
    let harness = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
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
fn water_invalid_configuration_marks_frame_failed_for_export() {
    let harness = harness::shared();
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
        &PrimitiveRegistry::with_builtin(),
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
    use manifold_renderer::node_graph::{bundled_preset_def, scene_exposure::metadata_for_node_type};

    let mut project = Project::default();
    let preset = PresetTypeId::new("SceneStarter");
    let baseline = bundled_preset_def(&preset).unwrap();
    let render_id = baseline.nodes.iter().find(|node| node.type_id == "node.render_scene").unwrap().id;
    let layer = Layer::new_generator("Fluid Authoring".into(), preset, 0);
    let target_graph = GraphTarget::Generator(layer.layer_id.clone());
    project.timeline.layers.push(layer);
    let mut add = AddSceneFluidCommand::new(target_graph.clone(), render_id,
        metadata_for_node_type("node.fluid_surface"), metadata_for_node_type("node.transform_3d"),
        metadata_for_node_type("node.pbr_material"), metadata_for_node_type("node.scene_object"),
        baseline.clone());
    add.execute(&mut project);
    assert!(add.was_applied(), "{:?}", add.rejection_reason());
    let saved = serde_json::to_string(&project).unwrap();
    let reloaded: Project = serde_json::from_str(&saved).unwrap();
    let graph = reloaded.graph_for_target(&target_graph, None).unwrap();
    let harness = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let build = |def| PresetRuntime::from_json_str_with_device(
        &serde_json::to_string(def).unwrap(), &registry, Arc::clone(&harness.device),
        WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, None).unwrap();
    let mut base_runtime = build(baseline);
    let mut fluid_runtime = build(graph);
    let target = RenderTarget::new(&harness.device, WIDTH, HEIGHT,
        GpuTextureFormat::Rgba16Float, "added-fluid-proof");
    let _offline = PhysicsStepScope::for_render(true);
    let base_pixels = render_frame(&mut base_runtime, &target, &harness.device, 0);
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
