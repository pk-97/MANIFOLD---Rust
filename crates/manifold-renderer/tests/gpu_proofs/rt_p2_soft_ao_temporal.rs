//! RAYTRACING_DESIGN.md §5.2 P2 gate — soft shadows + AO + temporal
//! accumulation with D3 resets.
//!
//! Two scripted, computed-number gates (no PNG oracles — Peter 2026-07-22,
//! §5.2 preamble), exercising `manifold_gpu::raytrace`'s
//! `accumulate_irradiance` kernel directly (the P2-specific piece; P1's
//! `rt_p1_shadow`/`rt_p4_metalfx_temporal` already prove the shared
//! accel/dispatch/upsample machinery this extends):
//!
//!   1. `cut_plus_one_matches_cold_start_within_epsilon` — the SAME numeric
//!      oracle shape as P4's cut-reset gate: a history texture warmed up on
//!      scene A, then "cut" (reset=true) to scene B's irradiance, must
//!      match a COLD-START accumulator seeing scene B for the first time
//!      (also reset=true) — no ghost of scene A survives.
//!   2. `strobe_retains_history_exceeds_epsilon` — D3's "strobes are not
//!      cuts": the SAME history texture, warmed on scene A, blended
//!      (reset=false) toward a light-intensity-flipped scene A' must
//!      DIFFER from a cold-start render of A' by MORE than a stated
//!      epsilon — proving history was retained (lagged toward A'), not
//!      discarded.
//!
//! Negative-rg gates (RT-D2, P2 brief): exactly one `TemporalResetDetector`
//! usage site for the reset (render_scene.rs's own accumulate call site);
//! GTAO dispatch absent from the RT-on path (neither `raytrace.rs` nor
//! `render_scene.rs` reference `ssao_gtao`/`SsaoGtao`) — both are static
//! `rg` facts checked at review time, not expressed as a test in this file.

use half::f16;
use manifold_gpu::raytrace::{AccumulateParams, MetalShadowRayTracer, ShadowRayTracer};
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureUsage};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;

use crate::harness::shared;

const W: u32 = 32;
const H: u32 = 32;

/// Blend weight used by these proofs — same committed range as
/// `render_scene.rs`'s `IRRADIANCE_ACCUM_ALPHA` (0.05-0.3); the exact
/// value doesn't matter to the reset/retain PROOF, only that it's neither
/// 0 nor 1 (both of which would degenerate the strobe case).
const TEST_ALPHA: f32 = 0.15;

/// Tight epsilon for the cut-reset proof: two `reset=true` writes of the
/// SAME constant content should agree almost exactly (f16 round-trip
/// tolerance only).
const RESET_EPSILON: f32 = 0.01;

/// A strobe's retained-history proof must exceed this — deliberately
/// smaller than `(1.0 - TEST_ALPHA) * |A - B|` for the fixture colors
/// below, so the assertion has real margin, not a coin flip.
const STROBE_RETAIN_EPSILON: f32 = 0.1;

fn flat_rgba_f16(w: u32, h: u32, r: f32, g: f32, b: f32) -> Vec<f16> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        out.push(f16::from_f32(r));
        out.push(f16::from_f32(g));
        out.push(f16::from_f32(b));
        out.push(f16::from_f32(0.0));
    }
    out
}

fn as_bytes<T>(v: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), std::mem::size_of_val(v)) }
}

fn upload_irr(device: &GpuDevice, r: f32, g: f32, b: f32, label: &str) -> GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width: W,
        height: H,
        depth: 1,
        format: manifold_gpu::GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
        label,
        mip_levels: 1,
    });
    device.upload_texture(&texture, as_bytes(&flat_rgba_f16(W, H, r, g, b)));
    texture
}

/// A history texture, freshly allocated (undefined content — every use
/// below either reset=true's into it first, or reads it only after a
/// prior write).
fn make_history(device: &GpuDevice, label: &str) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: W,
        height: H,
        depth: 1,
        format: manifold_gpu::GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
        label,
        mip_levels: 1,
    })
}

fn readback_rgba_f32(texture: &GpuTexture) -> Vec<f32> {
    let h = shared();
    let bytes_per_row = W * 8; // Rgba16Float = 8 bytes/px
    let total_bytes = u64::from(H * bytes_per_row);
    let buf = h.device.create_buffer_shared(total_bytes);
    let mut enc = h.device.create_encoder("p2-irradiance-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let f16s: &[f16] = unsafe { std::slice::from_raw_parts(ptr.cast::<f16>(), (W * H * 4) as usize) };
    f16s.iter().map(|v| v.to_f32()).collect()
}

fn mean_abs_diff_rgb(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    let mut sum = 0.0f32;
    let mut n = 0u32;
    for (i, (&av, &bv)) in a.iter().zip(b.iter()).enumerate() {
        if i % 4 == 3 {
            continue; // alpha unused by this texture's contract
        }
        sum += (av - bv).abs();
        n += 1;
    }
    sum / n as f32
}

fn run_accumulate(
    device: &GpuDevice,
    tracer: &MetalShadowRayTracer,
    hi_irr: &GpuTexture,
    history: &GpuTexture,
    alpha: f32,
    reset: bool,
    label: &str,
) {
    let params_buffer =
        device.create_buffer_shared(std::mem::size_of::<AccumulateParams>() as u64);
    let params = AccumulateParams::new([W, H], alpha, reset);
    let mut enc = device.create_encoder(label);
    {
        let gpu = RendererGpuEncoder::new(&mut enc, device);
        tracer.accumulate_irradiance(gpu.native_enc, &params, &params_buffer, hi_irr, history, label);
    }
    enc.commit_and_wait_completed();
}

#[test]
fn cut_plus_one_matches_cold_start_within_epsilon() {
    let h = shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    // Scene A: warm a history texture over several steady frames.
    let scene_a = upload_irr(&h.device, 0.8, 0.2, 0.1, "scene-a-irr");
    let history = make_history(&h.device, "p2-warmed-history");
    run_accumulate(&h.device, &tracer, &scene_a, &history, TEST_ALPHA, true, "p2-warm-frame-0");
    for i in 1..8 {
        run_accumulate(
            &h.device,
            &tracer,
            &scene_a,
            &history,
            TEST_ALPHA,
            false,
            &format!("p2-warm-frame-{i}"),
        );
    }

    // Cut: scene B's irradiance replaces scene A's on the SAME history
    // texture, with `reset: true` — the shared `TemporalResetDetector`
    // (wired in `render_scene.rs`) is what decides this bool in product
    // code; this test drives it directly to isolate the accumulate
    // kernel's own reset behavior.
    let scene_b = upload_irr(&h.device, 0.1, 0.6, 0.9, "scene-b-irr");
    run_accumulate(&h.device, &tracer, &scene_b, &history, TEST_ALPHA, true, "p2-cut-plus-one");
    let cut_plus_one = readback_rgba_f32(&history);

    // Cold start: a FRESH history texture's very first frame, same scene B
    // content, also reset=true.
    let cold_history = make_history(&h.device, "p2-cold-history");
    run_accumulate(&h.device, &tracer, &scene_b, &cold_history, TEST_ALPHA, true, "p2-cold-start");
    let cold_start = readback_rgba_f32(&cold_history);

    let diff = mean_abs_diff_rgb(&cut_plus_one, &cold_start);
    eprintln!("[P2] cut+1-vs-cold-start mean abs diff = {diff}");
    assert!(
        diff < RESET_EPSILON,
        "cut+1 frame still shows scene A's ghost (mean abs diff vs cold-start {diff} >= {RESET_EPSILON}) — reset did not discard history"
    );
}

#[test]
fn strobe_retains_history_exceeds_epsilon() {
    let h = shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    // Scene A: warm a history texture over several steady frames (same
    // clip, same owner_key in the real `render_scene.rs` integration).
    let scene_a = upload_irr(&h.device, 0.8, 0.2, 0.1, "scene-a-irr-strobe");
    let history = make_history(&h.device, "p2-strobe-history");
    run_accumulate(&h.device, &tracer, &scene_a, &history, TEST_ALPHA, true, "p2-strobe-warm-0");
    for i in 1..8 {
        run_accumulate(
            &h.device,
            &tracer,
            &scene_a,
            &history,
            TEST_ALPHA,
            false,
            &format!("p2-strobe-warm-{i}"),
        );
    }

    // Strobe: a light-intensity flip on the SAME clip — `reset: false`
    // (D3's "strobes are not cuts"; RT-D2's shared `TemporalResetDetector`
    // trips neither owner_key-change nor frame-time-discontinuity for a
    // same-clip intensity change, so product code passes `reset: false`
    // here too).
    let flipped = upload_irr(&h.device, 0.05, 0.05, 0.95, "scene-a-flipped-irr");
    run_accumulate(&h.device, &tracer, &flipped, &history, TEST_ALPHA, false, "p2-strobe-plus-one");
    let strobe_plus_one = readback_rgba_f32(&history);

    // Cold start: a FRESH history texture seeing the flipped value for the
    // first time (reset=true) — what the strobed frame would look like
    // WITHOUT retained history.
    let cold_history = make_history(&h.device, "p2-strobe-cold-history");
    run_accumulate(&h.device, &tracer, &flipped, &cold_history, TEST_ALPHA, true, "p2-strobe-cold-start");
    let cold_start = readback_rgba_f32(&cold_history);

    let diff = mean_abs_diff_rgb(&strobe_plus_one, &cold_start);
    eprintln!("[P2] strobe+1-vs-cold-start mean abs diff = {diff}");
    assert!(
        diff > STROBE_RETAIN_EPSILON,
        "strobe+1 frame matches a cold start too closely (mean abs diff {diff} <= {STROBE_RETAIN_EPSILON}) — history was NOT retained; a light-intensity flip is being treated as a cut"
    );
}
