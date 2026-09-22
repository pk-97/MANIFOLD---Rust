//! Complete PhysicsSolids graph proof.
//!
//! This renders the shipped preset through the production `PresetRuntime`,
//! including CPU Box3D simulation, compact Platonic mesh upload, scene
//! objects, PBR materials, camera, lights, and final output. The same runtime
//! is advanced at a fixed 1/60 second from frame 0 through frame 120 so the
//! readback comparison measures actual simulated motion rather than a fresh
//! runtime's initialization difference.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const PHYSICS_SOLIDS_JSON: &str = include_str!("../../assets/generator-presets/PhysicsSolids.json");
const FRAME_COUNT: u32 = 120;

fn render_frame(
    runtime: &mut PresetRuntime,
    target: &manifold_renderer::render_target::RenderTarget,
    frame: u32,
    width: u32,
    height: u32,
    device: &manifold_gpu::GpuDevice,
) {
    let seconds = frame as f64 / 60.0;
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
        frame_count: frame as i64,
        anim_progress: 0.0,
        trigger_count: 0,
    };

    let mut encoder = device.create_encoder("physics-solids-render");
    {
        let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
        runtime.render(
            &mut gpu,
            &target.texture,
            &context,
            &manifold_core::params::ParamManifest::default(),
        );
    }
    encoder.commit_and_wait_completed();
}

fn pixel_stats(bytes: &[u8]) -> (f64, f32) {
    let mut luma_sum = 0.0f64;
    let mut peak = 0.0f32;
    for pixel in bytes.chunks_exact(8) {
        let r = f16::from_le_bytes([pixel[0], pixel[1]]).to_f32();
        let g = f16::from_le_bytes([pixel[2], pixel[3]]).to_f32();
        let b = f16::from_le_bytes([pixel[4], pixel[5]]).to_f32();
        let a = f16::from_le_bytes([pixel[6], pixel[7]]).to_f32();
        assert!(
            r.is_finite() && g.is_finite() && b.is_finite() && a.is_finite(),
            "PhysicsSolids produced a non-finite pixel"
        );
        luma_sum += (0.2126 * r + 0.7152 * g + 0.0722 * b) as f64;
        peak = peak.max(r.max(g).max(b));
    }
    (luma_sum, peak)
}

fn mean_abs_diff(before: &[u8], after: &[u8]) -> f64 {
    assert_eq!(before.len(), after.len());
    let mut sum = 0.0f64;
    for (a, b) in before.chunks_exact(2).zip(after.chunks_exact(2)) {
        let av = f16::from_le_bytes([a[0], a[1]]).to_f32();
        let bv = f16::from_le_bytes([b[0], b[1]]).to_f32();
        assert!(av.is_finite() && bv.is_finite());
        sum += f64::from((av - bv).abs());
    }
    sum / (before.len() / 2) as f64
}

#[test]
fn physics_solids_renders_finite_nonempty_scene_and_moves() {
    let harness = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        PHYSICS_SOLIDS_JSON,
        &registry,
        std::sync::Arc::clone(&harness.device),
        harness.width,
        harness.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|error| panic!("PhysicsSolids graph must build: {error}"));
    let target = harness.make_target("physics-solids-proof");

    render_frame(
        &mut runtime,
        &target,
        0,
        harness.width,
        harness.height,
        &harness.device,
    );
    let initial = harness.readback(&target.texture);
    std::fs::write(
        "/tmp/physics_solids_initial.png",
        manifold_renderer::headless_readback::readback_to_srgb_png(
            &harness.device,
            &target.texture,
            harness.width,
            harness.height,
        ),
    )
    .unwrap();

    for frame in 1..=FRAME_COUNT {
        render_frame(
            &mut runtime,
            &target,
            frame,
            harness.width,
            harness.height,
            &harness.device,
        );
    }
    let settled = harness.readback(&target.texture);

    std::fs::write(
        "/tmp/physics_solids_settled.png",
        manifold_renderer::headless_readback::readback_to_srgb_png(
            &harness.device,
            &target.texture,
            harness.width,
            harness.height,
        ),
    )
    .unwrap();

    let (initial_luma, initial_peak) = pixel_stats(&initial);
    let (settled_luma, settled_peak) = pixel_stats(&settled);
    let motion = mean_abs_diff(&initial, &settled);
    eprintln!(
        "PhysicsSolids GPU proof: initial_luma={initial_luma:.3} settled_luma={settled_luma:.3} \
         initial_peak={initial_peak:.3} settled_peak={settled_peak:.3} mean_abs_diff={motion:.6} \
         artifacts=/tmp/physics_solids_initial.png,/tmp/physics_solids_settled.png"
    );

    assert!(initial_peak > 0.02, "initial PhysicsSolids frame is empty");
    assert!(settled_peak > 0.02, "settled PhysicsSolids frame is empty");
    assert!(
        motion > 0.0005,
        "120 simulated frames must change rendered pixels; mean_abs_diff={motion:.6}"
    );
}
