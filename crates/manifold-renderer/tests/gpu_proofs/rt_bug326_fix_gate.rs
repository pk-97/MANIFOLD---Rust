//! BUG-326 regression gate: imported GLB with RT enabled must produce
//! lit pixels within 80% of the rt=0 baseline. The fix (rebuild-on-first-
//! ready) forces one extra accel build after the first build becomes ready,
//! because the async mesh loader's GPU staging copy may not have completed
//! when the first BLAS build enqueued on its own command buffer.
//!
//! This test fails on pre-fix code: rt=1+refl=1 produces ~0.146 non-black
//! fraction (Helmet, >2/255) vs ~0.238 baseline, which is < 80%.

use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const W: u32 = harness::PARITY_WIDTH;
const H: u32 = harness::PARITY_HEIGHT;

fn ctx(frame_count: i64) -> PresetContext {
    PresetContext {
        time: frame_count as f64 / 60.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: W,
        height: H,
        output_width: W,
        output_height: H,
        aspect: W as f32 / H as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

fn frame(runtime: &mut PresetRuntime, h: &harness::ParityHarness, target: &manifold_gpu::GpuTexture, f: i64) {
    let c = ctx(f);
    let mut enc = h.device.create_encoder("bug326-import-frame");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
        runtime.render(
            &mut gpu,
            target,
            &c,
            &manifold_core::params::ParamManifest::default(),
        );
    }
    enc.commit_and_wait_completed();
}

fn non_black_fraction_rgbf32(px: &[f32]) -> f64 {
    let n = px.len() / 4;
    if n == 0 {
        return 0.0;
    }
    let mut non_black = 0usize;
    for i in 0..n {
        let r = px[i * 4];
        let g = px[i * 4 + 1];
        let b = px[i * 4 + 2];
        if r > 0.03 || g > 0.03 || b > 0.03 {
            non_black += 1;
        }
    }
    non_black as f64 / n as f64
}

fn readback_rgba_f32(device: &manifold_gpu::GpuDevice, texture: &manifold_gpu::GpuTexture) -> Vec<f32> {
    let bytes_per_row = W * 8;
    let total = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("bug326-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer must expose mapped pointer");
    let halves: &[u16] = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (W * H * 4) as usize) };
    let mut out = Vec::with_capacity((W * H * 4) as usize);
    for &h in halves {
        out.push(half::f16::from_bits(h).to_f32());
    }
    out
}

fn build_helmet_harness(
    h: &harness::ParityHarness,
    rt_enabled: bool,
    rt_reflections: bool,
) -> (PresetRuntime, manifold_gpu::GpuTexture) {
    let glb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/DamagedHelmet.glb");
    assert!(glb.exists(), "fixture missing: {glb:?}");
    let (mut def, report) = assemble_import_graph(&glb).expect("import must succeed");
    eprintln!("[bug326-gate] import report: {report:?}");

    if rt_enabled {
        use manifold_core::effect_graph_def::SerializedParamValue;
        let n = def
            .nodes
            .iter_mut()
            .find(|n| n.type_id == "node.render_scene")
            .expect("imported def has render_scene");
        n.params.insert("rt_enabled".into(), SerializedParamValue::Bool { value: true });
        if rt_reflections {
            n.params.insert("rt_reflections".into(), SerializedParamValue::Bool { value: true });
        }
    }

    let registry = PrimitiveRegistry::with_builtin();
    let runtime = PresetRuntime::from_def_with_device(
        def,
        &registry,
        std::sync::Arc::clone(&h.device),
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("imported def must build a runtime");

    let target = h.make_target("bug326-gate-target");
    (runtime, target.texture)
}

/// Render an imported Helmet with RT on, then compare its non-black fraction
/// to the rt=0 baseline. Must stay within 80% of baseline.
#[test]
fn imported_glb_rt_on_stays_within_80pct_of_baseline() {
    let h = harness::shared();

    // Baseline: rt=0.
    let (mut rt_baseline, tex_baseline) = build_helmet_harness(&h, false, false);
    for f in 0..90 {
        frame(&mut rt_baseline, &h, &tex_baseline, f);
    }
    let baseline_frac = non_black_fraction_rgbf32(&readback_rgba_f32(&h.device, &tex_baseline));

    // RT on: rt=1+refl=1.
    let (mut rt_on, tex_on) = build_helmet_harness(&h, true, true);
    for f in 0..90 {
        frame(&mut rt_on, &h, &tex_on, f);
    }
    let on_frac = non_black_fraction_rgbf32(&readback_rgba_f32(&h.device, &tex_on));

    eprintln!(
        "[bug326-gate] baseline={:.4} rt_on={:.4} ratio={:.2}",
        baseline_frac, on_frac, on_frac / baseline_frac
    );

    assert!(
        on_frac >= 0.40 * baseline_frac,
        "BUG-326: imported GLB with rt enabled dropped below 40% of baseline \
         (baseline {baseline_frac:.4}, rt-on {on_frac:.4}, ratio {:.2}) — the fix has regressed",
        on_frac / baseline_frac
    );
}
