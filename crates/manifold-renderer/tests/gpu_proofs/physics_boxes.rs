//! Production box demo: motion, reset-latched count, and zero-count rendering.
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::headless_readback::{readback_raw_halves, readback_to_srgb_png};
use manifold_renderer::node_graph::{PrimitiveRegistry, physics_metrics};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

const JSON: &str = include_str!("../../assets/generator-presets/PhysicsBoxes.json");

#[test]
fn physics_boxes_render_motion_and_latch_count_until_reset() {
    let harness = super::harness::shared();
    let device = &harness.device;
    let (width, height) = (640, 400);
    let def: EffectGraphDef = serde_json::from_str(JSON).unwrap();
    let mut params = ParamManifest::from_params(
        def.preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .cloned()
            .map(Param::bundled)
            .collect(),
    );
    let mut runtime = PresetRuntime::from_json_str_with_device(
        JSON,
        &PrimitiveRegistry::with_builtin(),
        std::sync::Arc::clone(device),
        width,
        height,
        GpuTextureFormat::Rgba16Float,
        Some(&params),
    )
    .expect("box demo compiles and installs");
    let target = RenderTarget::new(
        device,
        width,
        height,
        GpuTextureFormat::Rgba16Float,
        "physics-boxes-proof",
    );
    let mut render = |frame: u32, params: &ParamManifest| {
        let seconds = f64::from(frame) / 60.0;
        let context = PresetContext {
            time: seconds,
            beat: seconds,
            dt: 1.0 / 60.0,
            width,
            height,
            output_width: width,
            output_height: height,
            aspect: width as f32 / height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: i64::from(frame),
            anim_progress: 0.0,
            trigger_count: 0,
        };
        physics_metrics::begin_frame();
        let mut encoder = device.create_encoder("physics-boxes-proof");
        runtime.render(
            &mut GpuEncoder::new(&mut encoder, device),
            &target.texture,
            &context,
            params,
        );
        encoder.commit_and_wait_completed();
        physics_metrics::take_frame()
    };
    let first = render(0, &params);
    assert_eq!(
        first.body_count, 259,
        "256 boxes plus floor and two shared ramps"
    );
    let initial = readback_raw_halves(device, &target.texture, width, height);
    std::fs::write(
        "/tmp/physics_boxes_initial.png",
        readback_to_srgb_png(device, &target.texture, width, height),
    )
    .unwrap();
    for frame in 1..=180 {
        let metrics = render(frame, &params);
        assert_eq!(metrics.body_count, 259);
        assert!(metrics.physics_cpu_ms.is_finite() && metrics.physics_cpu_ms >= 0.0);
    }
    let dropped = readback_raw_halves(device, &target.texture, width, height);
    std::fs::write(
        "/tmp/physics_boxes_dropped.png",
        readback_to_srgb_png(device, &target.texture, width, height),
    )
    .unwrap();
    assert!(
        manifold_renderer::headless_readback::mean_abs_half_diff(&initial, &dropped) > 0.0005,
        "falling boxes must visibly move"
    );
    params.get_mut("40_copy_count").unwrap().value = 32.0;
    assert_eq!(
        render(181, &params).body_count,
        259,
        "slider edit does not rebuild"
    );
    params.get_mut("40_reset").unwrap().value = 1.0;
    assert_eq!(
        render(182, &params).body_count,
        35,
        "Reset applies requested count"
    );
    let sparse = readback_raw_halves(device, &target.texture, width, height);
    params.get_mut("40_copy_count").unwrap().value = 0.0;
    params.get_mut("40_reset").unwrap().value = 2.0;
    assert_eq!(
        render(183, &params).body_count,
        3,
        "zero leaves the floor and ramps only"
    );
    let floor = readback_raw_halves(device, &target.texture, width, height);
    assert!(
        manifold_renderer::headless_readback::mean_abs_half_diff(&sparse, &floor) > 0.00005,
        "zero instances must remove boxes from the rendered scene"
    );
    for bytes in floor.chunks_exact(2) {
        assert!(
            half::f16::from_le_bytes([bytes[0], bytes[1]])
                .to_f32()
                .is_finite()
        );
    }
    params.get_mut("40_copy_count").unwrap().value = 4_000.0;
    params.get_mut("40_reset").unwrap().value = 3.0;
    assert_eq!(render(184, &params).body_count, 4_003);
    assert_eq!(render(185, &params).body_count, 4_003);
    let full = readback_raw_halves(device, &target.texture, width, height);
    assert!(manifold_renderer::headless_readback::mean_abs_half_diff(&full, &floor) > 0.001);
    for bytes in full.chunks_exact(2) {
        assert!(
            half::f16::from_le_bytes([bytes[0], bytes[1]])
                .to_f32()
                .is_finite()
        );
    }
    std::fs::write(
        "/tmp/physics_boxes_4k.png",
        readback_to_srgb_png(device, &target.texture, width, height),
    )
    .unwrap();
    {
        let _live = manifold_renderer::node_graph::physics::PhysicsStepScope::for_render(false);
        assert_eq!(
            render(600, &params).body_count,
            4_003,
            "live stall recovers without Reset"
        );
        assert_eq!(
            render(601, &params).body_count,
            4_003,
            "next live frame still evaluates"
        );
    }
    eprintln!(
        "Physics Boxes: 256 -> pending 32 -> Reset 32 -> Reset 0 -> Reset 4000 verified; images /tmp/physics_boxes_initial.png and /tmp/physics_boxes_dropped.png"
    );
}
