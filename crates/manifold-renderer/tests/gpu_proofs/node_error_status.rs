//! A node that reports `ctx.error` fails the frame (BUG-hc5h, node ctx.error
//! does not fail the frame). The GPU FLIP Dam Break at a Resolution other than
//! the one its solver is built for errors from its domain and draws a
//! fallback; before the fix that frame still said Complete.

use std::sync::Arc;

use manifold_core::params::ParamManifest;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::frame_status::{FrameRenderFailure, FrameRenderStatus};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::node_graph::physics::PhysicsStepScope;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

use crate::harness;

const DAM_BREAK_JSON: &str = include_str!("../../assets/generator-presets/WaterDamBreakGpuFlip.json");
const WIDTH: u32 = 160;
const HEIGHT: u32 = 90;

#[test]
fn node_error_fails_the_frame() {
    let harness = harness::shared();
    let mut def: serde_json::Value = serde_json::from_str(DAM_BREAK_JSON).unwrap();
    let domain = def["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|node| node["nodeId"] == "domain")
        .unwrap();
    domain["params"]["resolution"]["value"] = serde_json::json!(32);
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
