//! A node that reports `ctx.error` fails the frame (BUG-hc5h).
//! Invalid Simulation Speed reaches the domain's runtime validation; unlike
//! Resolution, it cannot describe a valid dynamically sized solver lattice.

use std::sync::Arc;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::ParamManifest;
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::runtime::frame_status::{FrameRenderFailure, FrameRenderStatus};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::water::physics::PhysicsStepScope;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::gpu::render_target::RenderTarget;


const DAM_BREAK_JSON: &str = include_str!("../../assets/generator-presets/WaterDamBreakGpuFlip.json");
const WIDTH: u32 = 160;
const HEIGHT: u32 = 90;

#[test]
fn node_error_fails_the_frame() {
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let grouped: EffectGraphDef = serde_json::from_str(DAM_BREAK_JSON).unwrap();
    let mut def: serde_json::Value = serde_json::to_value(
        manifold_core::flatten::flatten_groups(&grouped).unwrap(),
    )
    .unwrap();
    let domain = def["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|node| node["nodeId"] == "domain")
        .unwrap();
    // Geometry remains valid, so loading succeeds. The domain's compute path
    // rejects speed outside 0..=4 and reports ctx.error during the frame.
    domain["params"]["speed"] = serde_json::json!({"type": "Float", "value": 5.0});
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &def.to_string(),
        &PrimitiveRegistry::with_builtin(),
        Arc::clone(&harness.device),
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap();
    let target = RenderTarget::new(&harness.device, WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, "node-error");
    let _offline = PhysicsStepScope::for_render(true);
    let context = PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: WIDTH,
        height: HEIGHT,
        output_width: WIDTH,
        output_height: HEIGHT,
        aspect: WIDTH as f32 / HEIGHT as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let mut encoder = harness.device.create_encoder("node-error");
    let status = {
        let mut gpu = RendererGpuEncoder::new(&mut encoder, &harness.device);
        runtime.render(&mut gpu, &target.texture, &context, &ParamManifest::default());
        gpu.frame_status()
    };
    encoder.commit_and_wait_completed();
    assert_eq!(status, FrameRenderStatus::Failed(FrameRenderFailure::NodeError));
}
