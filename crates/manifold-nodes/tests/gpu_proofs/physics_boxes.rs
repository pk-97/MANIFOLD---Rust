//! Production box demo: motion, reset-latched count, and zero-count rendering.
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::gpu::headless_readback::{readback_raw_halves, readback_to_srgb_png};
use manifold_water_rigid::physics::{SimStep, native_ticks_on_this_thread};
use {manifold_node_engine::persistence::PrimitiveRegistry, manifold_node_engine::exec::sim_metrics::SimMetrics};
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::gpu::render_target::RenderTarget;

const JSON: &str = include_str!("../../assets/generator-presets/PhysicsBoxes.json");

#[test]
fn physics_boxes_render_motion_and_latch_count_until_reset() {
    // Authored history must use the live clock from its first observation.
    let live = SimStep::live(manifold_core::Seconds(1.0 / 60.0));
    let step = std::cell::Cell::new(live);
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
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
        runtime.set_sim_step(step.get());
        let mut encoder = device.create_encoder("physics-boxes-proof");
        runtime.render(
            &mut GpuEncoder::new(&mut encoder, device),
            &target.texture,
            &context,
            params,
        );
        encoder.commit_and_wait_completed();
        let mut metrics = SimMetrics::default();
        runtime.drain_sim_metrics(&mut metrics);
        metrics
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
        assert!(metrics.cpu_ms.is_finite() && metrics.cpu_ms >= 0.0);
    }
    let dropped = readback_raw_halves(device, &target.texture, width, height);
    std::fs::write(
        "/tmp/physics_boxes_dropped.png",
        readback_to_srgb_png(device, &target.texture, width, height),
    )
    .unwrap();
    assert!(
        manifold_node_engine::gpu::headless_readback::mean_abs_half_diff(&initial, &dropped) > 0.0005,
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
        manifold_node_engine::gpu::headless_readback::mean_abs_half_diff(&sparse, &floor) > 0.00005,
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
    assert!(manifold_node_engine::gpu::headless_readback::mean_abs_half_diff(&full, &floor) > 0.001);
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
        step.set(live.with_preview_budget(std::time::Duration::ZERO));
        // Thirty owed intervals accept two; the other twenty-eight are dropped.
        let before_stall = native_ticks_on_this_thread();
        let stalled = render(215, &params);
        assert_eq!(stalled.body_count, 4_003);
        assert_eq!(native_ticks_on_this_thread() - before_stall, 2);
        assert!(
            (stalled.backlog_seconds - 28.0 / 60.0).abs() < 1e-6,
            "live overload reports the discarded time"
        );
        assert!(stalled.sim_step_cap_hit && !stalled.sim_nonfinite);
        let before_next = native_ticks_on_this_thread();
        let next = render(216, &params);
        assert_eq!(next.body_count, 4_003);
        assert_eq!(native_ticks_on_this_thread() - before_next, 1);
        assert!(
            next.backlog_seconds.abs() < 1e-6,
            "discarded time must not become catch-up debt on the next frame"
        );
        assert!(!next.sim_step_cap_hit && !next.sim_nonfinite);
        step.set(live);
    }
    eprintln!(
        "Physics Boxes: 256 -> pending 32 -> Reset 32 -> Reset 0 -> Reset 4000 verified; images /tmp/physics_boxes_initial.png and /tmp/physics_boxes_dropped.png"
    );
}
