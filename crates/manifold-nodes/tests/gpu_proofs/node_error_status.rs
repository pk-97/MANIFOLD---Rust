//! A node that reports `ctx.error` fails the frame (BUG-hc5h).
//! Invalid Simulation Speed reaches the domain's runtime validation; unlike
//! Resolution, it cannot describe a valid dynamically sized solver lattice.
//!
//! The graph is the smallest that keeps the GPU FLIP domain live: the domain
//! with the Dam Break's own params, its tick count read by a noise source that
//! reaches the final output (a node no root consumes never runs).

use std::sync::Arc;

use manifold_core::params::ParamManifest;
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::runtime::frame_status::{FrameRenderFailure, FrameRenderStatus};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::gpu::render_target::RenderTarget;

const WIDTH: u32 = 160;
const HEIGHT: u32 = 90;

/// The domain node as WaterDamBreakGpuFlip ships it, at `speed`.
fn domain_graph(speed: f32) -> String {
    serde_json::json!({
        "version": 1,
        "name": "NodeErrorDomain",
        "nodes": [
            { "id": 0, "typeId": "system.generator_input", "handle": "input", "nodeId": "input" },
            { "id": 1, "typeId": "node.gpu_flip_domain", "handle": "domain", "nodeId": "domain", "params": {
                "domain_size": { "type": "Float", "value": 4.0 },
                "fill_height": { "type": "Float", "value": 0.16 },
                "resolution": { "type": "Int", "value": 64 },
                "speed": { "type": "Float", "value": speed }
            } },
            { "id": 2, "typeId": "node.noise", "handle": "noise", "nodeId": "noise" },
            { "id": 3, "typeId": "system.final_output", "handle": "final_output", "nodeId": "final_output" }
        ],
        "wires": [
            { "fromNode": 1, "fromPort": "ticks", "toNode": 2, "toPort": "scale" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
        ]
    })
    .to_string()
}

fn first_frame_status(json: &str) -> FrameRenderStatus {
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        json,
        &PrimitiveRegistry::with_builtin(),
        Arc::clone(&harness.device),
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap();
    let target = RenderTarget::new(&harness.device, WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, "node-error");
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
    status
}

#[test]
fn node_error_fails_the_frame() {
    // Geometry remains valid, so loading succeeds. The domain's compute path
    // rejects speed outside 0..=4 and reports ctx.error during the frame.
    // The same graph at a valid speed must complete, so the failure below
    // comes from the speed check and not from the graph breaking elsewhere.
    assert_eq!(first_frame_status(&domain_graph(1.0)), FrameRenderStatus::Complete);
    let status = first_frame_status(&domain_graph(5.0));
    assert_eq!(status, FrameRenderStatus::Failed(FrameRenderFailure::NodeError));
}
