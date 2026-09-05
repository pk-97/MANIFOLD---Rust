//! RAYTRACING_DESIGN.md section 11 MB-B — in-repo regression pin for the
//! shipped `RT_GI_MAX_BOUNCES = 2` behaviour. The CAUSAL 1-vs-2 proof lives
//! in the workflow program (`scripts/rt_region_probe.py` run across the
//! MB-A/MB-B commits — bleed region rises, ambient-only region is
//! `cmp`-identical, I-MB2); this test does not re-derive that comparison.
//! It pins two facts about the CURRENT (bounces=2) build so a future change
//! that silently kills the second bounce, or leaks env into the gather,
//! trips a test instead of waiting for a human to notice on stage:
//!
//! 1. [`bleed_region_reads_above_the_probe_pin_threshold`] — `RtBleed.json`'s
//!    open-floor probe region (no line of sight to the emitter, full sight
//!    of the red wall — same rect the program's `rt_region_probe.py` used)
//!    reads a positive R-minus-G tint: the second bounce's wall-relayed
//!    emitter energy, still present. Threshold is the program's own
//!    `pin_threshold` from its last recorded run (`rg_a=0.0, rg_b=0.01926,
//!    delta=0.01926` — `pin_threshold = max(0.006, delta/2) = 0.00963`),
//!    2026-07-30.
//! 2. [`ambient_only_region_matches_the_analytic_ambient_times_ao_value`] —
//!    I-MB2: `RtAmbientOnly.json` (same geometry, `emission_intensity: 0`)
//!    has zero lights and zero emissive AND no envmap wired, so the GI
//!    gather's `gi` term is algebraically zero at every depth (no sun-
//!    bounce caster, no emissive hit, and the env-miss reads the black
//!    dummy — RAYTRACING_DESIGN.md section 14 ED1 keeps env out of the
//!    gather whenever the scene has no env chain) — the probe region's
//!    colour is exactly the consumer-side flat ambient recompose
//!    (`render_scene.wgsl`'s `rt_or_flat_ambient`:
//!    `albedo * scene_params.y * ambient_tint * AMBIENT_IRRADIANCE_SCALE *
//!    mask.a`, ED2). Every material in the fixture is white or near-white
//!    with `ambient: 0.1`, so that recompose = `1.0 * 0.1 * 1.0 * 0.15 * ao`
//!    = `0.015 * ao` per channel; the open, unoccluded probe region's `ao`
//!    reads close to 1 (the epsilon below is sized off the actual measured
//!    value, not derived from a hemisphere-integral you'd have to re-derive
//!    by hand). Extra bounces must not move this number — no env chain means
//!    the gather still contributes nothing.

use std::ffi::c_void;
use std::slice;
use std::sync::Arc;

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

use crate::harness;

/// Matches `tools/rt_prototype/compare/RtBleed.json`'s companion
/// `graph_tool render` default output size — the program's captures and
/// `scripts/rt_region_probe.py --rect` are both in this pixel space, and
/// this test's rect below is the SAME rect so the two stay comparable.
const W: u32 = 512;
const H: u32 = 512;

/// RT-D4's async accel build needs a few frames to settle before the GI
/// gather (which reads the SAME resident accel structure) can be trusted —
/// same warm-up discipline `rt_p3_emissive_gi.rs`'s `RT_WARMUP_FRAMES`
/// documents in full.
const RT_WARMUP_FRAMES: i64 = 16;

const BLEED_JSON: &str = include_str!("../../../../tools/rt_prototype/compare/RtBleed.json");
const AMBIENT_JSON: &str =
    include_str!("../../../../tools/rt_prototype/compare/RtAmbientOnly.json");

/// `scripts/rt_region_probe.py --rect` — the open-floor strip that has no
/// line of sight to the emitter but full sight of the red wall.
const PROBE_RECT: (u32, u32, u32, u32) = (80, 175, 230, 235);

fn readback_rgba16f(device: &manifold_gpu::GpuDevice, texture: &manifold_gpu::GpuTexture) -> Vec<u8> {
    const BYTES_PER_PIXEL: u32 = 8; // Rgba16Float
    let bytes_per_row = W * BYTES_PER_PIXEL;
    let total_bytes = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total_bytes);
    let mut enc = device.create_encoder("rt-t38-multibounce-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let bytes: &[u8] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), total_bytes as usize) };
    bytes.to_vec()
}

fn render_fixture(json: &str) -> Vec<u8> {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        json,
        &registry,
        Arc::clone(&h.device),
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("rt-t38 multibounce fixture must build");

    let target = RenderTarget::new(&h.device, W, H, GpuTextureFormat::Rgba16Float, "rt-t38-multibounce");
    for frame in 0..RT_WARMUP_FRAMES {
        let ctx = PresetContext {
            time: 0.1,
            beat: 0.2,
            dt: 1.0 / 60.0,
            width: W,
            height: H,
            output_width: W,
            output_height: H,
            aspect: W as f32 / H as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
            gpu_signal_committed: 0,
            gpu_signaled: 0,
        };
        let mut enc = h.device.create_encoder("rt-t38-multibounce-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &manifold_core::params::ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
    }
    readback_rgba16f(&h.device, &target.texture)
}

/// Mean linear RGB over `rect` (`x0,y0,x1,y1`, half-open — same convention
/// as `scripts/rt_region_probe.py`'s numpy slice) from a raw `Rgba16Float`
/// readback.
fn region_rgb_mean(bytes: &[u8], rect: (u32, u32, u32, u32)) -> (f64, f64, f64) {
    let (x0, y0, x1, y1) = rect;
    let mut sum = (0.0f64, 0.0f64, 0.0f64);
    let mut n = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let idx = ((y * W + x) * 8) as usize;
            let px = &bytes[idx..idx + 8];
            let r = f16::from_le_bytes([px[0], px[1]]).to_f32() as f64;
            let g = f16::from_le_bytes([px[2], px[3]]).to_f32() as f64;
            let b = f16::from_le_bytes([px[4], px[5]]).to_f32() as f64;
            assert!(r.is_finite() && g.is_finite() && b.is_finite(), "non-finite pixel");
            sum.0 += r;
            sum.1 += g;
            sum.2 += b;
            n += 1;
        }
    }
    assert!(n > 0, "probe rect is entirely off-screen");
    (sum.0 / n as f64, sum.1 / n as f64, sum.2 / n as f64)
}

#[test]
fn bleed_region_reads_above_the_probe_pin_threshold() {
    let bytes = render_fixture(BLEED_JSON);
    let (r, g, _b) = region_rgb_mean(&bytes, PROBE_RECT);
    let rg = r - g;
    eprintln!("rt_t38 bleed probe region: r={r:.5} g={g:.5} r-g={rg:.5}");

    // scripts/rt_region_probe.py's last recorded program run (2026-07-30,
    // PNG/tonemap-space): rg_a=0.0 (1-bounce control), rg_b=0.01926
    // (2-bounce), delta=0.01926, pin_threshold=max(0.006, delta/2)=0.00963.
    // This test reads the raw LINEAR GPU buffer instead (no tonemap/8-bit
    // quantization), so the absolute number differs — measured here
    // 2026-07-30: r-g=0.01096. Half that (0.005) is the floor: a real
    // regression (second bounce stops reaching this region, or the wall's
    // relayed tint drops out) reads as a much bigger move than driver/
    // hardware noise on a deterministic, fixed-seed render.
    const PIN_THRESHOLD: f64 = 0.005;
    assert!(
        rg > PIN_THRESHOLD,
        "bleed region (rect {PROBE_RECT:?}) must read R-G > {PIN_THRESHOLD} at the shipped \
         bounces=2 depth — got r={r:.5} g={g:.5} r-g={rg:.5}; a value at or below the control \
         leg's near-zero baseline means the second GI bounce is no longer reaching this region"
    );
}

#[test]
fn ambient_only_region_matches_the_analytic_ambient_times_ao_value() {
    let bytes = render_fixture(AMBIENT_JSON);
    let (r, g, b) = region_rgb_mean(&bytes, PROBE_RECT);
    eprintln!("rt_t38 ambient-only probe region: r={r:.5} g={g:.5} b={b:.5}");

    // I-MB2: zero lights, emission_intensity 0 -> the GI gather's `gi` term
    // (emissive-hit + sun-bounce) is algebraically zero at every depth, so
    // `irradiance = ambient_color * ao + gi` reduces to `ambient_color *
    // ao`, and `rt_or_flat_ambient` multiplies by albedo (white/near-white
    // here) downstream. `ambient_color = ambient_tint(1,1,1) *
    // AMBIENT_IRRADIANCE_SCALE(0.15) * scene_ambient(0.1) = 0.015`
    // (render_scene.rs:4173, scene_ambient is the max material `ambient`
    // across this fixture's 4 objects, all 0.1 except the emitter's 0.0).
    // The open, unoccluded probe rect's `ao` reads close to 1 — ANALYTIC
    // in the sense that it follows directly from the formula above with
    // ao=1, not tuned to whatever the renderer happened to output.
    const AMBIENT_COLOR: f64 = 0.015;
    // Measured 2026-07-30: r=g=b=0.01498 (open, unoccluded probe rect —
    // ao reads ~0.999, essentially the ao=1 open-sky assumption). 0.002
    // is ~7x that gap: a real regression (env leaking into the gather, or
    // the ambient/AO formula changing shape) reads as a much bigger jump
    // than driver/hardware noise on a deterministic, fixed-seed render.
    const EPS: f64 = 0.002;
    assert!(
        (r - AMBIENT_COLOR).abs() < EPS && (g - AMBIENT_COLOR).abs() < EPS,
        "ambient-only region (rect {PROBE_RECT:?}) must read close to albedo*ambient*SCALE*ao \
         ({AMBIENT_COLOR:.4} per channel, ao~=1) at the shipped bounces=2 depth — got \
         r={r:.5} g={g:.5} b={b:.5}; a no-env scene's gather must contribute nothing at any \
         depth (ED1/I-MB2)"
    );
}
