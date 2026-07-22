//! RAYTRACING_DESIGN.md §5.2 P4 gate — MetalFX Temporal upscaling.
//!
//! Three scripted, computed-number gates (no PNG oracles — Peter 2026-07-22,
//! §5.2 preamble):
//!   1. `temporal_scaler_produces_exact_target_resolution` — the scaler's
//!      output texture is exactly `dst_w x dst_h`, not "close to".
//!   2. `upscaled_output_approximates_native_within_coarse_epsilon` —
//!      upscaling a half-res render of a known analytic gradient
//!      approximates the SAME gradient rendered natively at full res
//!      (proves it upscales the scene, not garbage).
//!   3. `cut_plus_one_matches_cold_start_within_epsilon` — the SAME numeric
//!      oracle shape as P2's cut-reset gate: a scaler with warmed-up
//!      history from scene A, cut to scene B via
//!      `crate::node_graph::temporal_reset::TemporalResetDetector` (the
//!      SHARED reset-detection path — RT-D2), must produce output
//!      indistinguishable from a COLD-START scaler seeing scene B for the
//!      first time. Proves no ghost of scene A survives the reset.

use half::f16;
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureUsage};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::metalfx_temporal_upscaler::MetalFxTemporalUpscaler;
use manifold_renderer::node_graph::FrameTime;
use manifold_renderer::node_graph::temporal_reset::TemporalResetDetector;

use crate::harness::shared;

const SRC_W: u32 = 64;
const SRC_H: u32 = 64;
const DST_W: u32 = 128;
const DST_H: u32 = 128;

/// Coarse epsilon for "upscales the scene, not garbage" — generous on
/// purpose (MetalFX's proprietary ML reconstruction is not bit-exact to a
/// naive analytic upsample; quality judgment is Peter's morning call, not
/// this gate's).
const UPSCALE_COARSE_EPSILON: f32 = 0.15;

/// Tighter epsilon for the cut-reset proof: two `reset=true` encodes of
/// the SAME source content should agree almost exactly, whether or not
/// the scaler instance has unrelated prior history.
const RESET_EPSILON: f32 = 0.02;

/// Analytic RGB gradient sampled at cell centers of a `w x h` grid —
/// the CPU oracle this whole file compares against. Deterministic,
/// resolution-independent (same formula at any `w x h`), so "native
/// render at full res" and "downsampled render at half res" are both just
/// this function evaluated at different grids — no separate renderer
/// needed to get a trustworthy low-res source.
fn gradient_rgba_f16(w: u32, h: u32) -> Vec<f16> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let r = (x as f32 + 0.5) / w as f32;
            let g = (y as f32 + 0.5) / h as f32;
            out.push(f16::from_f32(r));
            out.push(f16::from_f32(g));
            out.push(f16::from_f32(0.5));
            out.push(f16::from_f32(1.0));
        }
    }
    out
}

/// Flat depth (`R32Float`) — a static, non-deforming scene: every pixel
/// the same device-depth value.
fn flat_depth_f32(w: u32, h: u32, value: f32) -> Vec<f32> {
    vec![value; (w * h) as usize]
}

/// Zero motion (`Rg16Float`) — no camera/object motion between frames,
/// matching this test's static-content fixtures.
fn zero_motion_f16(w: u32, h: u32) -> Vec<f16> {
    vec![f16::from_f32(0.0); (w * h * 2) as usize]
}

fn upload_input(device: &GpuDevice, w: u32, h: u32, fmt: manifold_gpu::GpuTextureFormat, bytes: &[u8], label: &str) -> GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format: fmt,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
        label,
        mip_levels: 1,
    });
    device.upload_texture(&texture, bytes);
    texture
}

fn as_bytes<T>(v: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), std::mem::size_of_val(v)) }
}

/// Read back an `Rgba16Float` texture as `f32` RGBA, row-major top-down.
fn readback_rgba_f32(texture: &GpuTexture, w: u32, h: u32) -> Vec<f32> {
    let h_harness = shared();
    let bytes_per_row = w * 8; // Rgba16Float = 8 bytes/px
    let total_bytes = u64::from(h * bytes_per_row);
    let buf = h_harness.device.create_buffer_shared(total_bytes);
    let mut enc = h_harness.device.create_encoder("p4-temporal-readback");
    enc.copy_texture_to_buffer(texture, &buf, w, h, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let f16s: &[f16] = unsafe {
        std::slice::from_raw_parts(ptr.cast::<f16>(), (w * h * 4) as usize)
    };
    f16s.iter().map(|v| v.to_f32()).collect()
}

fn mean_abs_diff_rgb(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    let mut sum = 0.0f32;
    let mut n = 0u32;
    // Compare RGB only (skip alpha, index % 4 == 3) — the visually
    // meaningful channels for this proof.
    for (i, (&av, &bv)) in a.iter().zip(b.iter()).enumerate() {
        if i % 4 == 3 {
            continue;
        }
        sum += (av - bv).abs();
        n += 1;
    }
    sum / n as f32
}

/// A fully-built scaler + input fixtures for one "scene": the analytic
/// gradient rendered at `SRC_W x SRC_H`, flat depth, zero motion.
struct SceneFixture {
    color: GpuTexture,
    depth: GpuTexture,
    motion: GpuTexture,
}

fn build_scene(device: &GpuDevice, depth_value: f32, label: &str) -> SceneFixture {
    let color = upload_input(
        device,
        SRC_W,
        SRC_H,
        manifold_gpu::GpuTextureFormat::Rgba16Float,
        as_bytes(&gradient_rgba_f16(SRC_W, SRC_H)),
        &format!("{label}-color"),
    );
    let depth = upload_input(
        device,
        SRC_W,
        SRC_H,
        manifold_gpu::GpuTextureFormat::R32Float,
        as_bytes(&flat_depth_f32(SRC_W, SRC_H, depth_value)),
        &format!("{label}-depth"),
    );
    let motion = upload_input(
        device,
        SRC_W,
        SRC_H,
        manifold_gpu::GpuTextureFormat::Rg16Float,
        as_bytes(&zero_motion_f16(SRC_W, SRC_H)),
        &format!("{label}-motion"),
    );
    SceneFixture { color, depth, motion }
}

#[test]
fn temporal_scaler_produces_exact_target_resolution() {
    let h = shared();
    let Some(upscaler) = MetalFxTemporalUpscaler::new(&h.device, SRC_W, SRC_H, DST_W, DST_H) else {
        eprintln!("[SKIP] MetalFX Temporal not available on this device/OS");
        return;
    };
    assert_eq!(upscaler.output.width, DST_W, "output texture width must be the exact target resolution");
    assert_eq!(upscaler.output.height, DST_H, "output texture height must be the exact target resolution");
    assert_eq!(upscaler.dst_w, DST_W);
    assert_eq!(upscaler.dst_h, DST_H);
}

#[test]
fn upscaled_output_approximates_native_within_coarse_epsilon() {
    let h = shared();
    let Some(upscaler) = MetalFxTemporalUpscaler::new(&h.device, SRC_W, SRC_H, DST_W, DST_H) else {
        eprintln!("[SKIP] MetalFX Temporal not available on this device/OS");
        return;
    };
    let scene = build_scene(&h.device, 0.5, "upscale-proof");

    let mut enc = h.device.create_encoder("p4-temporal-upscale-proof");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
        // First-ever frame on a fresh scaler: reset=true (no history).
        upscaler.upscale(&mut gpu, &scene.color, &scene.depth, &scene.motion, 0.0, 0.0, true);
    }
    enc.commit_and_wait_completed();

    let upscaled = readback_rgba_f32(&upscaler.output.texture, DST_W, DST_H);
    let native = gradient_rgba_f16(DST_W, DST_H)
        .into_iter()
        .map(|v| v.to_f32())
        .collect::<Vec<_>>();

    let diff = mean_abs_diff_rgb(&upscaled, &native);
    eprintln!("[P4] upscaled-vs-native mean abs diff = {diff}");
    assert!(
        diff < UPSCALE_COARSE_EPSILON,
        "upscaled output diverges too far from the native-res render (mean abs diff {diff} >= {UPSCALE_COARSE_EPSILON}) — looks like garbage, not an upscale"
    );
}

#[test]
fn cut_plus_one_matches_cold_start_within_epsilon() {
    let h = shared();
    let Some(warmed) = MetalFxTemporalUpscaler::new(&h.device, SRC_W, SRC_H, DST_W, DST_H) else {
        eprintln!("[SKIP] MetalFX Temporal not available on this device/OS");
        return;
    };
    let Some(cold) = MetalFxTemporalUpscaler::new(&h.device, SRC_W, SRC_H, DST_W, DST_H) else {
        eprintln!("[SKIP] MetalFX Temporal not available on this device/OS");
        return;
    };

    // Scene A: warm the `warmed` scaler's history up over several frames —
    // the same shared `TemporalResetDetector` P2's accumulator will reuse
    // decides reset=true only on frame 0 (owner_key 1, first-ever frame).
    let scene_a = build_scene(&h.device, 0.5, "scene-a");
    let mut resets = TemporalResetDetector::new();
    let dt = 1.0 / 60.0;
    for i in 0..8u32 {
        let t = f64::from(i) * dt;
        let frame = FrameTime {
            beats: manifold_core::Beats(0.0),
            seconds: manifold_core::Seconds(t),
            delta: manifold_core::Seconds(dt),
            frame_count: i64::from(i),
        };
        let reset = resets.detect_reset(1, &frame);
        assert_eq!(reset, i == 0, "only frame 0 (cold start) should reset within scene A");
        let mut enc = h.device.create_encoder("p4-warm-scene-a");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            warmed.upscale(&mut gpu, &scene_a.color, &scene_a.depth, &scene_a.motion, 0.0, 0.0, reset);
        }
        enc.commit_and_wait_completed();
    }

    // Cut: scene B replaces scene A on the SAME node (owner_key changes
    // 1 -> 2). The shared detector must flag this as a reset.
    let scene_b = build_scene(&h.device, 0.5, "scene-b");
    let cut_frame = FrameTime {
        beats: manifold_core::Beats(0.0),
        seconds: manifold_core::Seconds(8.0 * dt),
        delta: manifold_core::Seconds(dt),
        frame_count: 8,
    };
    let reset_at_cut = resets.detect_reset(2, &cut_frame);
    assert!(reset_at_cut, "owner_key change (the cut) must trip the shared reset detector");

    let mut enc = h.device.create_encoder("p4-cut-plus-one");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
        warmed.upscale(&mut gpu, &scene_b.color, &scene_b.depth, &scene_b.motion, 0.0, 0.0, reset_at_cut);
    }
    enc.commit_and_wait_completed();
    let cut_plus_one = readback_rgba_f32(&warmed.output.texture, DST_W, DST_H);

    // Cold start: a FRESH scaler's very first frame, same scene B content.
    let mut enc = h.device.create_encoder("p4-cold-start");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
        cold.upscale(&mut gpu, &scene_b.color, &scene_b.depth, &scene_b.motion, 0.0, 0.0, true);
    }
    enc.commit_and_wait_completed();
    let cold_start = readback_rgba_f32(&cold.output.texture, DST_W, DST_H);

    let diff = mean_abs_diff_rgb(&cut_plus_one, &cold_start);
    eprintln!("[P4] cut+1-vs-cold-start mean abs diff = {diff}");
    assert!(
        diff < RESET_EPSILON,
        "cut+1 frame still shows scene A's ghost (mean abs diff vs cold-start {diff} >= {RESET_EPSILON}) — reset did not discard history"
    );
}
