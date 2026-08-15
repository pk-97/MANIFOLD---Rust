//! BUG-majv (rt-kernel-toggle-sequence-breaks-raster-base-state) — Peter's
//! in-app repro: RT off → on with default kernels → disable all four RT
//! kernels → RT off → RT on. RT enabled with every kernel off must be the
//! raster image, but three per-kernel READ gates in `render_scene` were
//! wider than the kernels' WRITE conditions (`rt_flags.y` ignored `rt_gi`,
//! `rt_flags.z` latched on `rt_ready` and ignored `rt_shadows`, the ambient
//! AO read had no kernel gate at all), so the off→on plan recompile left
//! freshly-reallocated RT textures being read as if live — sun visibility
//! from a zeroed `rt_sun_tint`, diffuse from a zeroed irradiance `.rgb`,
//! ambient from a zeroed `.a`. Raster base gone; dotted outline + halos.
//!
//! The sequence is driven through the CARD manifest (the app-faithful path —
//! Peter's UI flips are card writes, and `import_rt_manifest`'s doc is why
//! node-param writes don't reach the card-owned RT toggles). The final
//! RT-on/kernels-off frame is asserted against the RT-off raster reference
//! at the SAME camera angle.
//!
//! Two harness traps this test is shaped around:
//! - Rerun suppression windows (documented by the bug326 gate): an
//!   rt_enabled flip triggers a rerun whose async build suppresses the
//!   composite, which reads exactly black on a fresh target (in-app the
//!   previous frame persists). Window length is load-dependent, so every
//!   phase polls until lit instead of rendering a fixed frame count.
//! - The texture pool hands the recompiled plan the SAME textures back with
//!   still-valid converged content, so on a static camera the pre-fix
//!   build's stale reads are the right values and the test passes vacuously
//!   (measured: luma ratio 0.94, diff 0.014 pre-fix). Orbiting the camera
//!   after the kernel disable makes every converged accumulation stale for
//!   the new view, which is what makes the pre-fix read gates visible in
//!   principle.
//!
//! Honest discrimination note (measured 2026-08-15, k3): even with the
//! orbit, this harness does NOT fail on pre-fix source — the in-place plan
//! recompile reuses pool textures, so the stale channels stay plausible
//! (the GI substitution is a matched estimator of the raster irradiance by
//! design, and AO is low-frequency; only zeroed channels corrupt visibly).
//! Peter's app corruption needed the app's fresh-allocation lifecycle
//! (zeroed/garbage RT textures after the off→on rebuild), which this
//! harness's pool never produces. The app-level verification is the
//! rt-capture run of Peter's exact sequence (broken on main, correct with
//! the fix). This test still gates the invariant — RT-on + all-kernels-off
//! must equal the raster image, before and after an off→on cycle, with a
//! camera move making any stale channel wrong — and starts discriminating
//! the day the pool stops reusing (zeroed channels fail it loudly).

use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const W: u32 = harness::PARITY_WIDTH;
const H: u32 = harness::PARITY_HEIGHT;

/// Non-black coverage of the healthy render at this fixture size is 0.0709
/// (measured on a quiet machine). The suppression window reads exactly
/// black, so any threshold well under the settled coverage discriminates.
const LIT_FRACTION_THRESHOLD: f64 = 0.03;
/// Same budget class as the bug326 gate's load-dependent wait.
const POLL_BUDGET_FRAMES: i64 = 600;

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
        owner_key: 7,
        is_clip_level: false,
        frame_count,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

fn frame(
    runtime: &mut PresetRuntime,
    h: &harness::ParityHarness,
    target: &manifold_gpu::GpuTexture,
    f: i64,
    manifest: &manifold_core::params::ParamManifest,
) {
    let c = ctx(f);
    let mut enc = h.device.create_encoder("bugmajv-frame");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
        runtime.render(&mut gpu, target, &c, manifest);
    }
    enc.commit_and_wait_completed();
}

/// Card write by id suffix — the same resolution `import_rt_manifest` uses.
fn set_card(manifest: &mut manifold_core::params::ParamManifest, suffix: &str, on: bool) {
    set_card_value(manifest, suffix, if on { 1.0 } else { 0.0 });
}

fn set_card_value(manifest: &mut manifold_core::params::ParamManifest, suffix: &str, value: f32) {
    let id = manifest
        .iter()
        .find(|p| p.id().ends_with(suffix))
        .map(|p| p.id().to_string())
        .unwrap_or_else(|| panic!("imported def exposes no card param ending `{suffix}`"));
    manifest
        .get_mut(&id)
        .expect("id came from this manifest")
        .value = value;
}

fn card_value(manifest: &manifold_core::params::ParamManifest, suffix: &str) -> f32 {
    manifest
        .iter()
        .find(|p| p.id().ends_with(suffix))
        .map(|p| p.value)
        .unwrap_or_else(|| panic!("imported def exposes no card param ending `{suffix}`"))
}

fn non_black_fraction(px: &[f32]) -> f64 {
    let n = px.len() / 4;
    let mut lit = 0usize;
    for i in 0..n {
        let l = px[i * 4] as f64 + px[i * 4 + 1] as f64 + px[i * 4 + 2] as f64;
        if l > 0.012 {
            lit += 1;
        }
    }
    lit as f64 / n as f64
}

fn mean_luma(px: &[f32]) -> f64 {
    let mut sum = 0.0f64;
    let n = px.len() / 4;
    for i in 0..n {
        let (r, g, b) = (px[i * 4] as f64, px[i * 4 + 1] as f64, px[i * 4 + 2] as f64);
        sum += 0.2126 * r + 0.7152 * g + 0.0722 * b;
    }
    sum / n as f64
}

fn mean_abs_diff(a: &[f32], b: &[f32]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mut sum = 0.0f64;
    let n = a.len() / 4;
    for i in 0..n * 4 {
        sum += (a[i] as f64 - b[i] as f64).abs();
    }
    sum / (n * 4) as f64
}

/// Render until the composite comes out of the rerun suppression window
/// (reads exactly black on this harness's fresh target; in-app the previous
/// frame persists). Returns the first lit frame's pixels and its frame index.
fn render_until_lit(
    runtime: &mut PresetRuntime,
    h: &harness::ParityHarness,
    target: &manifold_gpu::GpuTexture,
    manifest: &manifold_core::params::ParamManifest,
    start_frame: i64,
    phase: &str,
) -> (Vec<f32>, i64) {
    let mut px = Vec::new();
    let mut frac = 0.0f64;
    let mut f = start_frame;
    while f < start_frame + POLL_BUDGET_FRAMES {
        frame(runtime, h, target, f, manifest);
        // Settle a few frames past the flip, then check every 5th.
        if f >= start_frame + 4 && (f - start_frame) % 5 == 4 {
            px = crate::rt_t2b_temporal_wiring::readback_rgba_f32(&h.device, target);
            frac = non_black_fraction(&px);
            if frac >= LIT_FRACTION_THRESHOLD {
                return (px, f);
            }
        }
        f += 1;
    }
    panic!(
        "bug-majv {phase}: never lit within {POLL_BUDGET_FRAMES} frames (best non-black fraction {frac:.4})"
    );
}

#[test]
fn rt_kernel_toggle_sequence_preserves_raster_base() {
    // DamagedHelmet (the bug326 gate's fixture): solid surfaces, so the
    // RT-on jitter floor is tiny silhouette-edge error and the stale-channel
    // signal stands out. The sparse-blossom apricot was hopeless here —
    // sub-pixel jitter on 1px branches moves whole-frame luma by ~20%,
    // exactly the size of the stale-read error (identical pre/post-fix
    // numbers measured: 0.0511 vs 0.0653 both builds).
    let glb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/DamagedHelmet.glb");
    assert!(glb.exists(), "fixture missing: {glb:?}");
    let h = harness::shared();
    let (def, report) = assemble_import_graph(&glb).expect("helmet import must succeed");
    eprintln!("[bug-majv] import report: {report:?}");

    // Build RT-on with all kernels at their default (on) — Peter's state
    // after the first off→on.
    let mut manifest = harness::import_rt_manifest(&def, true, true);
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_def_with_device(
        def,
        &registry,
        std::sync::Arc::clone(&h.device),
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        Some(&manifest),
    )
    .expect("helmet def must build a runtime");
    harness::assert_no_shadowed_def_params(&runtime, "bug-majv helmet import");

    let target = h.make_target("bugmajv-helmet");
    // Converge the RT accumulation with all kernels on at view A.
    let (_px, mut f) = render_until_lit(&mut runtime, h, &target.texture, &manifest, 0, "converge");
    // Anti-vacuity: the RT kernel really dispatched.
    harness::assert_rt_dispatched(
        || frame(&mut runtime, h, &target.texture, f + 1, &manifest),
        "bug-majv",
    );
    f += 2;

    // Disable all four kernels (RT still on), then orbit the camera. The RT
    // channels are screen-space, so every converged accumulation is stale
    // for view B — on a static camera the pre-fix build passes vacuously
    // (pool texture reuse keeps the stale content valid; measured luma
    // ratio 0.94). See the module doc's discrimination note for what this
    // test does and doesn't prove pre-fix.
    for suffix in ["_rt_shadows", "_rt_ao", "_rt_gi", "_rt_reflections"] {
        set_card(&mut manifest, suffix, false);
    }
    let orbit = card_value(&manifest, "_orbit");
    set_card_value(&mut manifest, "_orbit", orbit + 0.9);

    // All four comparisons happen at view B. RT on + kernels off must be
    // exactly the raster image, before and after the off→on cycle.
    let (kernels_off, next) = render_until_lit(&mut runtime, h, &target.texture, &manifest, f, "kernels_off");
    f = next + 1;

    set_card(&mut manifest, "_rt_enabled", false);
    let (raster_ref, next) = render_until_lit(&mut runtime, h, &target.texture, &manifest, f, "raster_ref");
    f = next + 1;

    set_card(&mut manifest, "_rt_enabled", true);
    let (resurrected, next) = render_until_lit(&mut runtime, h, &target.texture, &manifest, f, "resurrected");
    f = next + 1;

    set_card(&mut manifest, "_rt_enabled", false);
    let (raster_ref2, _next) = render_until_lit(&mut runtime, h, &target.texture, &manifest, f, "raster_ref2");

    let luma_ref = mean_luma(&raster_ref);
    let luma_ref2 = mean_luma(&raster_ref2);
    let luma_kernels_off = mean_luma(&kernels_off);
    let luma_resurrected = mean_luma(&resurrected);
    // Per-pixel diff against the SAME-view raster reference. rt_enabled
    // jitters the projection sub-pixel and the readbacks land on different
    // jitter phases, so the post-fix floor is nonzero (silhouette edges of
    // the sparse blossoms) — the bound is calibrated, not zero.
    let diff_kernels_off = mean_abs_diff(&kernels_off, &raster_ref);
    let diff_resurrected = mean_abs_diff(&resurrected, &raster_ref2);
    eprintln!(
        "[bug-majv] luma: raster_ref {luma_ref:.4}, kernels_off {luma_kernels_off:.4}, resurrected {luma_resurrected:.4}, raster_ref2 {luma_ref2:.4}; \
         diff: kernels_off {diff_kernels_off:.4}, resurrected {diff_resurrected:.4}"
    );
    assert!(luma_ref > 1e-3, "raster reference must not be black ({luma_ref})");
    assert!(luma_ref2 > 1e-3, "second raster reference must not be black ({luma_ref2})");
    for (label, luma, reference) in [
        ("kernels_off", luma_kernels_off, luma_ref),
        ("resurrected", luma_resurrected, luma_ref2),
    ] {
        let ratio = luma / reference;
        assert!(
            (0.7..=1.4).contains(&ratio),
            "BUG-majv: {label} frame lost the raster base — mean luma {luma:.4} vs same-view raster {reference:.4} (ratio {ratio:.2}); \
             a kernel read gate is wider than its kernel's write condition"
        );
    }
    // The discriminator: each RT-on/kernels-off frame must BE the raster
    // image of the current view. Catches any regression where a dead
    // (zeroed/unwritten) channel is read; the stale-but-valid case is
    // second-order by design (module doc's discrimination note).
    for (label, diff) in [("kernels_off", diff_kernels_off), ("resurrected", diff_resurrected)] {
        assert!(
            diff < 0.02,
            "BUG-majv: {label} frame is not the raster image — mean_abs_diff vs same-view raster {diff:.4}; \
             a kernel read gate is wider than its kernel's write condition"
        );
    }
}
