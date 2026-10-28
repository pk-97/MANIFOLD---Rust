//! Ray tracing comes back on its own after an update it could not make.
//! A needs-preparation answer prepares and traces the same frame; a real
//! failure draws that frame raster and the next frame traces again. Faults
//! come from the encoder's one-shot `fail_rt_update`.

use std::sync::Arc;

use manifold_gpu::GpuTextureFormat;
use manifold_gpu::raytrace::RtAccelError;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::gpu::render_target::RenderTarget;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::frame_status::{FrameRenderFailure, FrameRenderStatus};
use manifold_node_engine::runtime::preset_context::PresetContext;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;

struct Frame {
    status: FrameRenderStatus,
    traced: bool,
}

fn render(
    device: &Arc<manifold_gpu::GpuDevice>,
    runtime: &mut PresetRuntime,
    target: &RenderTarget,
    frame: usize,
    fault: Option<RtAccelError>,
) -> Frame {
    let ctx = PresetContext {
        time: frame as f64 / 60.0,
        beat: frame as f64 / 30.0,
        dt: 1.0 / 60.0,
        width: WIDTH,
        height: HEIGHT,
        output_width: WIDTH,
        output_height: HEIGHT,
        aspect: WIDTH as f32 / HEIGHT as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: frame as i64,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let mut encoder = device.create_encoder("rt-update-recovery");
    let result = {
        let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
        gpu.fail_rt_update = fault;
        runtime.render(
            &mut gpu,
            &target.texture,
            &ctx,
            &manifold_core::params::ParamManifest::default(),
        );
        Frame {
            status: gpu.frame_status(),
            traced: gpu.rt_dispatches > 0,
        }
    };
    encoder.commit_and_wait().expect("GPU work completes");
    result
}

#[test]
fn ray_tracing_comes_back_after_a_frame_it_could_not_update() {
    let test_device = manifold_gpu::testkit::test_device();
    let device = test_device.arc();
    let registry = PrimitiveRegistry::with_builtin();
    let json = include_str!("../fixtures/scene-modifiers/rt_dynamic_reference.json");
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &manifold_nodes_scene::testkit::scene_objects::bundle_scene_objects(json),
        &registry,
        Arc::clone(&device),
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("RT reference scene compiles");
    let target = RenderTarget::new(
        &device,
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        "rt-update-recovery",
    );

    let mut frame = 0;
    let mut next = |runtime: &mut PresetRuntime, fault| {
        let rendered = render(&device, runtime, &target, frame, fault);
        frame += 1;
        rendered
    };
    let warm = (0..8).map(|_| next(&mut runtime, None)).last().unwrap();
    assert!(
        warm.status == FrameRenderStatus::Complete && warm.traced,
        "the scene traces before any fault"
    );

    // Needs preparation: the renderer prepares and traces the same frame.
    let prepared = next(&mut runtime, Some(RtAccelError::NeedsPreparation));
    let after_prepared = next(&mut runtime, None);

    // A real failure: raster this frame, said once; tracing the next.
    let failed = next(
        &mut runtime,
        Some(RtAccelError::Allocation {
            bytes: 1 << 40,
            resource: "fault",
        }),
    );
    let after_failed: Vec<Frame> = (0..3).map(|_| next(&mut runtime, None)).collect();

    assert!(
        prepared.status == FrameRenderStatus::Complete && prepared.traced,
        "a needs-preparation frame prepares and traces in place, got {:?} traced={}",
        prepared.status,
        prepared.traced,
    );
    assert!(after_prepared.status == FrameRenderStatus::Complete && after_prepared.traced);
    assert_eq!(
        failed.status,
        FrameRenderStatus::Failed(FrameRenderFailure::NodeError),
        "a failed update draws raster and says so"
    );
    assert!(!failed.traced, "a failed update draws that frame raster");
    for (index, frame) in after_failed.iter().enumerate() {
        assert!(
            frame.status == FrameRenderStatus::Complete && frame.traced,
            "frame {index} after a failure traces again, got {:?} traced={}",
            frame.status,
            frame.traced,
        );
    }
}
