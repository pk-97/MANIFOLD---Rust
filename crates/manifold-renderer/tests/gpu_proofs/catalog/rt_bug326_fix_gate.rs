//! BUG-326 regression gate: imported GLB with RT enabled must produce
//! lit pixels within 20% of the rt=0 baseline. Covers the structural
//! import+RT path (gltf_import -> PresetRuntime). The async-load race
//! itself (BLAS built over pre-load zero buffers because the staging
//! copy and the BLAS build are on separate command buffers) is not
//! reliably reproducible in-harness. Background decode must settle before
//! comparing either arm; frame counts alone do not establish readiness.
//! The fix (rebuild-on-first-ready with per-topology
//! rerun) is verified via render-import 50ms-paced traces (BUG-326
//! entry: Helmet frame2=0.146->frame3+=0.239, AMG 0.045->0.168).

use manifold_gpu::{GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;

use crate::harness;

const W: u32 = 512;
const H: u32 = 512;

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

/// The card manifest drives every frame. An EMPTY manifest is what made the
/// original version of this gate vacuous: it set `rt_enabled` on the def's
/// `render_scene` node, which the card binding overwrote at build (BUG-1l7f), so
/// both arms rendered pure raster and the ratio assert passed trivially.
fn frame(
    runtime: &mut PresetRuntime,
    h: &manifold_node_engine::testkit::gpu_harness::ParityHarness,
    target: &manifold_gpu::GpuTexture,
    f: i64,
    manifest: &manifold_core::params::ParamManifest,
) {
    let c = ctx(f);
    // A commit can be an InnocentVictim of a shared-GPU contention transient
    // (BUG-m0c9); re-rendering the same idempotent frame absorbs it. A real
    // wedge still panics after the single retry.
    manifold_node_engine::testkit::gpu_harness::retry_on_gpu_commit_error(|| {
        let mut enc = h.device.create_encoder("bug326-import-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, target, &c, manifest);
        }
        enc.commit_and_wait_completed();
    });
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

fn make_512_target(device: &GpuDevice, label: &str) -> manifold_gpu::GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: W,
        height: H,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::RENDER_TARGET_FULL,
        label,
        mip_levels: 1,
    })
}

fn build_helmet_harness(
    h: &manifold_node_engine::testkit::gpu_harness::ParityHarness,
    rt_enabled: bool,
    rt_reflections: bool,
) -> (
    PresetRuntime,
    manifold_gpu::GpuTexture,
    manifold_core::params::ParamManifest,
) {
    let glb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/DamagedHelmet.glb");
    assert!(glb.exists(), "fixture missing: {glb:?}");
    let (def, report) = assemble_import_graph(&glb).expect("import must succeed");
    eprintln!("[bug326-gate] import report: {report:?}");

    // The card manifest is the only route that reaches the RT block — the
    // import promoted `render_scene`'s RT toggles to outer card params, so a
    // write onto the node itself is reverted at build (BUG-1l7f).
    let manifest = harness::import_rt_manifest(&def, rt_enabled, rt_reflections);

    let registry = PrimitiveRegistry::with_builtin();
    let runtime = PresetRuntime::from_def_with_device(
        def,
        &registry,
        std::sync::Arc::clone(&h.device),
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        Some(&manifest),
    )
    .expect("imported def must build a runtime");
    manifold_node_engine::testkit::gpu_harness::assert_no_shadowed_def_params(&runtime, "bug326 helmet import");

    let target = make_512_target(&h.device, "bug326-gate-target");
    (runtime, target, manifest)
}

/// Render an imported Helmet with RT on, then compare its non-black fraction
/// to the rt=0 baseline. Must stay within 80% of baseline.
#[test]
fn imported_glb_rt_on_stays_within_80pct_of_baseline() {
    let h = manifold_node_engine::testkit::gpu_harness::shared();

    // Baseline: rt=0.
    // Frames rendered while the import is still loading don't count toward
    // either arm's budget (the load's length depends on machine load).
    const SETTLED_FRAME_BUDGET: u32 = 600;
    let (mut rt_baseline, tex_baseline, base_manifest) = build_helmet_harness(h, false, false);
    let wait = manifold_node_engine::testkit::gpu_harness::BackgroundWait::new("bug326 baseline");
    let mut baseline_frac = 0.0;
    let mut settled = 0;
    for f in 0i64.. {
        frame(&mut rt_baseline, h, &tex_baseline, f, &base_manifest);
        if wait.pending(&rt_baseline) {
            continue;
        }
        settled += 1;
        if f >= 89 {
            // Quiescent loaders can become ready on a frame whose composite
            // was suppressed during preparation. Require a visible completed
            // frame, just as the RT arm below does, within the same bound.
            baseline_frac = non_black_fraction_rgbf32(&readback_rgba_f32(&h.device, &tex_baseline));
            if baseline_frac > 0.0 {
                break;
            }
        }
        if settled >= SETTLED_FRAME_BUDGET {
            break;
        }
    }
    assert!(baseline_frac > 0.0, "baseline must contain lit pixels, not a vacuous zero threshold");

    // RT on: rt=1+refl=1. Poll until lit: the rerun suppression window
    // skips the composite while the rerun build is in flight, which reads
    // back as black on this harness's fresh target (in-app the previous
    // frame persists). Window length is load-dependent (completion-handler
    // delivery), so a fixed frame count is flaky under full-suite load.
    let (mut rt_on, tex_on, on_manifest) = build_helmet_harness(h, true, true);
    let wait = manifold_node_engine::testkit::gpu_harness::BackgroundWait::new("bug326 rt-on");
    let threshold = 0.20 * baseline_frac;
    let mut on_frac = 0.0f64;
    let mut settled = 0;
    let mut f = 0i64;
    while settled < SETTLED_FRAME_BUDGET {
        frame(&mut rt_on, h, &tex_on, f, &on_manifest);
        f += 1;
        if wait.pending(&rt_on) {
            continue;
        }
        settled += 1;
        let rendered = f - 1;
        if rendered >= 84 && rendered % 5 == 4 {
            on_frac = on_frac.max(non_black_fraction_rgbf32(&readback_rgba_f32(&h.device, &tex_on)));
            if on_frac >= threshold {
                break;
            }
        }
    }

    eprintln!(
        "[bug326-gate] baseline={:.4} rt_on={:.4} ratio={:.2}",
        baseline_frac, on_frac, on_frac / baseline_frac
    );

    // Liveness: the RT kernel really dispatched on the rt-on arm. Capture slots
    // are only pushed from inside `render_scene`'s `rt_enabled && rt_ready`
    // branch, so a non-empty capture reads the mechanism directly — without this
    // a pure-raster arm can satisfy the ratio assert below and report nothing.
    harness::assert_rt_dispatched(
        || frame(&mut rt_on, h, &tex_on, f, &on_manifest),
        "bug326 rt-on arm",
    );

    assert!(
        on_frac >= threshold,
        "BUG-326: imported GLB with rt enabled never lit within {SETTLED_FRAME_BUDGET} settled frames \
         (baseline {baseline_frac:.4}, rt-on {on_frac:.4}, ratio {:.2}) — the fix has regressed",
        on_frac / baseline_frac
    );
}
