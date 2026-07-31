//! RAYTRACING_DESIGN.md section 5.2 P2 gate — soft shadows + AO + temporal
//! accumulation with D3 resets.
//!
//! Two scripted, computed-number gates (no PNG oracles — Peter 2026-07-22,
//! section 5.2 preamble), exercising `manifold_gpu::raytrace`'s
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
use manifold_gpu::raytrace::{AccumulateParams, GiMaterial, MetalShadowRayTracer, ShadowRayTracer};
use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;

use crate::harness::shared;

/// RT-T1-C (BUG-311): `accumulate_irradiance` now reprojects through
/// `inv_view_proj`/`prev_view_proj` — IDENTITY for both makes the
/// reprojected texel equal the current texel exactly (this test's fixture
/// has no real camera), so this proof's cut/strobe semantics are unchanged
/// from the pre-reprojection same-texel behavior.
const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

const W: u32 = 32;
const H: u32 = 32;

/// Blend weight used by these proofs — same committed range as
/// `render_scene.rs`'s `IRRADIANCE_ACCUM_ALPHA` (0.05-0.3); the exact
/// value doesn't matter to the reset/retain PROOF, only that it's neither
/// 0 nor 1 (both of which would degenerate the strobe case).
const TEST_ALPHA: f32 = 0.15;

/// The kernel blends at `1/n` (n = frames of history behind the texel),
/// floored at `TEST_ALPHA` — a running mean, so a still surface converges
/// instead of sitting at a fixed noise floor. Every proof below that
/// retains history does exactly ONE frame after a reset, and a texel with
/// one prior sample weights the new one at 1/2. Raise the frame count and
/// this becomes 1/3, 1/4, ... until the floor bites.
const SECOND_FRAME_ALPHA: f32 = 0.5;

/// Tight epsilon for the cut-reset proof: two `reset=true` writes of the
/// SAME constant content should agree almost exactly (f16 round-trip
/// tolerance only).
const RESET_EPSILON: f32 = 0.01;

/// A strobe's retained-history proof must exceed this — deliberately
/// smaller than `(1.0 - TEST_ALPHA) * |A - B|` for the fixture colors
/// below, so the assertion has real margin, not a coin flip.
const STROBE_RETAIN_EPSILON: f32 = 0.1;

fn flat_rgba_f16_with_a(w: u32, h: u32, r: f32, g: f32, b: f32, a: f32) -> Vec<f16> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        out.push(f16::from_f32(r));
        out.push(f16::from_f32(g));
        out.push(f16::from_f32(b));
        out.push(f16::from_f32(a));
    }
    out
}

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

/// RT-T1-C: a constant depth texture (this fixture has no real camera/
/// geometry) — with `IDENTITY` view-proj matrices the reprojected texel is
/// always the current texel, so a CONSTANT depth/normal everywhere makes
/// the validity test pass unconditionally, same as this proof's pre-
/// reprojection same-texel assumption.
fn make_constant_depth(device: &GpuDevice, label: &str) -> GpuTexture {
    make_depth_at(device, 0.5, label)
}

/// RT-T2-C: same constant-depth fixture at a caller-chosen NDC z — the
/// object-motion proof encodes an object's motion as a depth change.
fn make_depth_at(device: &GpuDevice, z: f32, label: &str) -> GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width: W,
        height: H,
        depth: 1,
        format: GpuTextureFormat::Depth32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
        label,
        mip_levels: 1,
    });
    let pixels = vec![z; (W * H) as usize];
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), std::mem::size_of_val(&pixels[..])) };
    device.upload_texture(&texture, bytes);
    texture
}

/// RT-T1-C: a constant world-space up-normal texture, same "no real camera"
/// discipline as `make_constant_depth` above.
fn make_constant_normal(device: &GpuDevice, label: &str) -> GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width: W,
        height: H,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
        label,
        mip_levels: 1,
    });
    device.upload_texture(&texture, as_bytes(&flat_rgba_f16(W, H, 0.0, 1.0, 0.0)));
    texture
}

/// BUG-322: a constant normal texture carrying an arbitrary direction (and
/// object id 0 in `.w`, which `flat_rgba_f16` writes as alpha 0.0) — the
/// rotating-object proof needs two different orientations of the same
/// surface.
fn make_object_normal(device: &GpuDevice, n: [f32; 3], label: &str) -> GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width: W,
        height: H,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
        label,
        mip_levels: 1,
    });
    device.upload_texture(&texture, as_bytes(&flat_rgba_f16(W, H, n[0], n[1], n[2])));
    texture
}

/// RT-T1-C: a depth/normal HISTORY channel, read_write-capable but always
/// used as a strict ping-pong pair — `SHADER_READ` when it's this frame's
/// read source, `SHADER_WRITE` when it's this frame's write target, never
/// both roles on the same texture in the same dispatch.
fn make_history_side_channel(device: &GpuDevice, format: GpuTextureFormat, label: &str) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: W,
        height: H,
        depth: 1,
        format,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::SHADER_READ,
        label,
        mip_levels: 1,
    })
}

/// RT-T1-C: the irradiance history plus its depth/normal side channels,
/// each a ping-pong PAIR (`accumulate_irradiance`'s read/write textures
/// must be distinct — see the kernel's own doc comment on why a single
/// read_write texture would race). `advance()` after each dispatch flips
/// which slot is "read" vs "write" for the next call.
struct HistorySet {
    irr: [GpuTexture; 2],
    depth: [GpuTexture; 2],
    normal: [GpuTexture; 2],
    /// RT-T1-D (BUG-312): luminance-moments ping-pong pair — this test
    /// doesn't assert on variance, just needs valid bindings for
    /// `accumulate_irradiance`'s widened signature.
    moments: [GpuTexture; 2],
    /// RT-R2 (RD6): specular history ping-pong pair — same lifecycle as
    /// irr history pair above (inert pass-through at this step).
    refl: [GpuTexture; 2],
    /// SV-ACCUM: shadow-visibility history ping-pong pair — same ping
    /// clock as every other channel.
    sv: [GpuTexture; 2],
    /// SV-ACCUM moments: per-channel first/second visibility moments.
    sv_m1: [GpuTexture; 2],
    sv_m2: [GpuTexture; 2],
    /// SV-ACCUM snap-hold countdown pair (`.x`) — a gate trip holds the
    /// n=2 snap for 4 frames (the straddling moments deaden the sigma
    /// gate right after a real crossing).
    sv_hold: [GpuTexture; 2],
    ping: usize,
}

impl HistorySet {
    fn new(device: &GpuDevice, label: &str) -> Self {
        Self {
            irr: [
                make_history(device, &format!("{label}-irr-a")),
                make_history(device, &format!("{label}-irr-b")),
            ],
            depth: [
                make_history_side_channel(device, GpuTextureFormat::R32Float, &format!("{label}-depth-a")),
                make_history_side_channel(device, GpuTextureFormat::R32Float, &format!("{label}-depth-b")),
            ],
            normal: [
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, &format!("{label}-normal-a")),
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, &format!("{label}-normal-b")),
            ],
            moments: [
                make_history_side_channel(device, GpuTextureFormat::Rgba32Float, &format!("{label}-moments-a")),
                make_history_side_channel(device, GpuTextureFormat::Rgba32Float, &format!("{label}-moments-b")),
            ],
            // RT-R2 (RD6): inert pass-through refl history pair — same
            // Rgba16Float format as irr history.
            refl: [
                make_history(device, &format!("{label}-refl-a")),
                make_history(device, &format!("{label}-refl-b")),
            ],
            sv: [
                make_history(device, &format!("{label}-sv-a")),
                make_history(device, &format!("{label}-sv-b")),
            ],
            sv_m1: [
                make_history(device, &format!("{label}-sv-m1-a")),
                make_history(device, &format!("{label}-sv-m1-b")),
            ],
            sv_m2: [
                make_history(device, &format!("{label}-sv-m2-a")),
                make_history(device, &format!("{label}-sv-m2-b")),
            ],
            sv_hold: [
                make_history(device, &format!("{label}-sv-hold-a")),
                make_history(device, &format!("{label}-sv-hold-b")),
            ],
            ping: 0,
        }
    }
    fn read_irr(&self) -> &GpuTexture {
        &self.irr[self.ping]
    }
    fn write_irr(&self) -> &GpuTexture {
        &self.irr[1 - self.ping]
    }
    fn read_depth(&self) -> &GpuTexture {
        &self.depth[self.ping]
    }
    fn write_depth(&self) -> &GpuTexture {
        &self.depth[1 - self.ping]
    }
    fn read_normal(&self) -> &GpuTexture {
        &self.normal[self.ping]
    }
    fn write_normal(&self) -> &GpuTexture {
        &self.normal[1 - self.ping]
    }
    fn read_moments(&self) -> &GpuTexture {
        &self.moments[self.ping]
    }
    fn write_moments(&self) -> &GpuTexture {
        &self.moments[1 - self.ping]
    }
    // RT-R2 (RD6): specular history read/write — inert pass-through
    // at this step, same ping clock as all other history channels.
    fn read_refl(&self) -> &GpuTexture {
        &self.refl[self.ping]
    }
    fn write_refl(&self) -> &GpuTexture {
        &self.refl[1 - self.ping]
    }
    // SV-ACCUM: visibility history read/write — same ping clock.
    fn read_sv(&self) -> &GpuTexture {
        &self.sv[self.ping]
    }
    fn write_sv(&self) -> &GpuTexture {
        &self.sv[1 - self.ping]
    }
    // SV-ACCUM moments: visibility moments read/write — same ping clock.
    fn read_sv_m1(&self) -> &GpuTexture {
        &self.sv_m1[self.ping]
    }
    fn write_sv_m1(&self) -> &GpuTexture {
        &self.sv_m1[1 - self.ping]
    }
    fn read_sv_m2(&self) -> &GpuTexture {
        &self.sv_m2[self.ping]
    }
    fn write_sv_m2(&self) -> &GpuTexture {
        &self.sv_m2[1 - self.ping]
    }
    // SV-ACCUM snap-hold countdown — same ping clock.
    fn read_sv_hold(&self) -> &GpuTexture {
        &self.sv_hold[self.ping]
    }
    fn write_sv_hold(&self) -> &GpuTexture {
        &self.sv_hold[1 - self.ping]
    }
    fn advance(&mut self) {
        self.ping = 1 - self.ping;
    }
    /// The most recently written irradiance texture — call AFTER
    /// `advance()`, matching `self.ping`'s new value.
    fn current_irr(&self) -> &GpuTexture {
        &self.irr[self.ping]
    }
    /// The most recently written visibility texture — same contract as
    /// `current_irr`.
    fn current_sv(&self) -> &GpuTexture {
        &self.sv[self.ping]
    }
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
    depth_tex: &GpuTexture,
    hi_normal: &GpuTexture,
    history: &mut HistorySet,
    alpha: f32,
    reset: bool,
    label: &str,
) {
    // RT-T2-C: zero objects — every pixel reprojects camera-only, exactly
    // the pre-object-motion behavior this test's expectations encode.
    run_accumulate_with_motion(
        device, tracer, hi_irr, depth_tex, hi_normal, history, alpha, reset, 0, IDENTITY, IDENTITY,
        label,
    );
}

/// RT-T2-C: `run_accumulate` with an explicit object-motion table — one
/// delta matrix, `obj_count` objects (0 = camera-only reprojection for
/// every pixel regardless of the id channel's content) — and, for the
/// BUG-ukg camera-motion proof, an explicit `prev_view_proj` (IDENTITY =
/// exact self-reprojection, the pre-camera-motion fixture behavior).
#[allow(clippy::too_many_arguments)] // un-suppress: collapse into a params struct if this harness gains a 13th knob
fn run_accumulate_with_motion(
    device: &GpuDevice,
    tracer: &MetalShadowRayTracer,
    hi_irr: &GpuTexture,
    depth_tex: &GpuTexture,
    hi_normal: &GpuTexture,
    history: &mut HistorySet,
    alpha: f32,
    reset: bool,
    obj_count: u32,
    obj_motion: [[f32; 4]; 4],
    prev_view_proj: [[f32; 4]; 4],
    label: &str,
) {
    // SV-ACCUM: fully-lit visibility for the current frame (1 = no shadow)
    // — the tests here assert on the irr/refl channels, so a constant
    // pass-through sv is the honest inert input.
    let hi_sv_lit = upload_irr(device, 1.0, 1.0, 1.0, "p2-hi-sv-lit");
    run_accumulate_with_sv(
        device, tracer, hi_irr, depth_tex, hi_normal, history, alpha, reset, obj_count,
        obj_motion, prev_view_proj, &hi_sv_lit, label,
    );
}

/// `run_accumulate` with an explicit shadow-visibility input — the
/// SV-ACCUM proofs below drive a flickering sv; the pre-existing proofs
/// get the constant fully-lit dummy via `run_accumulate`.
#[allow(clippy::too_many_arguments)]
fn run_accumulate_with_sv(
    device: &GpuDevice,
    tracer: &MetalShadowRayTracer,
    hi_irr: &GpuTexture,
    depth_tex: &GpuTexture,
    hi_normal: &GpuTexture,
    history: &mut HistorySet,
    alpha: f32,
    reset: bool,
    obj_count: u32,
    obj_motion: [[f32; 4]; 4],
    prev_view_proj: [[f32; 4]; 4],
    hi_sv: &GpuTexture,
    label: &str,
) {
    let params_buffer =
        device.create_buffer_shared(std::mem::size_of::<AccumulateParams>() as u64);
    let params =
        AccumulateParams::new([W, H], alpha, reset, obj_count, [0.0; 3], IDENTITY, prev_view_proj);
    let obj_motion_buffer =
        device.create_buffer_shared(std::mem::size_of::<[[f32; 4]; 4]>() as u64);
    {
        let ptr = obj_motion_buffer
            .mapped_ptr()
            .expect("obj-motion buffer must be CPU-mapped (create_buffer_shared)");
        unsafe {
            std::ptr::copy_nonoverlapping(
                obj_motion.as_ptr() as *const u8,
                ptr,
                std::mem::size_of::<[[f32; 4]; 4]>(),
            );
        }
    }
    let hi_refl_dummy = upload_irr(device, 0.0, 0.0, 0.0, "p2-hi-refl-dummy");
    let gi_materials_buf = device.create_buffer_shared(std::mem::size_of::<GiMaterial>() as u64);
    let mut enc = device.create_encoder(label);
    {
        let gpu = RendererGpuEncoder::new(&mut enc, device);
        tracer.accumulate_irradiance(
            gpu.native_enc,
            &params,
            &params_buffer,
            &obj_motion_buffer,
            hi_irr,
            depth_tex,
            hi_normal,
            history.read_irr(),
            history.write_irr(),
            history.read_depth(),
            history.write_depth(),
            history.read_normal(),
            history.write_normal(),
            history.read_moments(),
            history.write_moments(),
            &hi_refl_dummy,
            history.read_refl(),
            history.write_refl(),
            &gi_materials_buf,
            hi_sv,
            history.read_sv(),
            history.write_sv(),
            history.read_sv_m1(),
            history.write_sv_m1(),
            history.read_sv_m2(),
            history.write_sv_m2(),
            history.read_sv_hold(),
            history.write_sv_hold(),
            label,
        );
    }
    enc.commit_and_wait_completed();
    history.advance();
}

#[test]
fn cut_plus_one_matches_cold_start_within_epsilon() {
    let h = shared();
    let tracer = MetalShadowRayTracer::new(&h.device);
    let depth_tex = make_constant_depth(&h.device, "p2-depth");
    let hi_normal = make_constant_normal(&h.device, "p2-normal");

    // Scene A: warm a history texture over several steady frames.
    let scene_a = upload_irr(&h.device, 0.8, 0.2, 0.1, "scene-a-irr");
    let mut history = HistorySet::new(&h.device, "p2-warmed-history");
    run_accumulate(&h.device, &tracer, &scene_a, &depth_tex, &hi_normal, &mut history, TEST_ALPHA, true, "p2-warm-frame-0");
    for i in 1..8 {
        run_accumulate(
            &h.device,
            &tracer,
            &scene_a,
            &depth_tex,
            &hi_normal,
            &mut history,
            TEST_ALPHA,
            false,
            &format!("p2-warm-frame-{i}"),
        );
    }

    // Cut: scene B's irradiance replaces scene A's on the SAME history
    // set, with `reset: true` — the shared `TemporalResetDetector` (wired
    // in `render_scene.rs`) is what decides this bool in product code;
    // this test drives it directly to isolate the accumulate kernel's own
    // reset behavior.
    let scene_b = upload_irr(&h.device, 0.1, 0.6, 0.9, "scene-b-irr");
    run_accumulate(&h.device, &tracer, &scene_b, &depth_tex, &hi_normal, &mut history, TEST_ALPHA, true, "p2-cut-plus-one");
    let cut_plus_one = readback_rgba_f32(history.current_irr());

    // Cold start: a FRESH history set's very first frame, same scene B
    // content, also reset=true.
    let mut cold_history = HistorySet::new(&h.device, "p2-cold-history");
    run_accumulate(&h.device, &tracer, &scene_b, &depth_tex, &hi_normal, &mut cold_history, TEST_ALPHA, true, "p2-cold-start");
    let cold_start = readback_rgba_f32(cold_history.current_irr());

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
    let depth_tex = make_constant_depth(&h.device, "p2-strobe-depth");
    let hi_normal = make_constant_normal(&h.device, "p2-strobe-normal");

    // Scene A: warm a history texture over several steady frames (same
    // clip, same owner_key in the real `render_scene.rs` integration).
    let scene_a = upload_irr(&h.device, 0.8, 0.2, 0.1, "scene-a-irr-strobe");
    let mut history = HistorySet::new(&h.device, "p2-strobe-history");
    run_accumulate(&h.device, &tracer, &scene_a, &depth_tex, &hi_normal, &mut history, TEST_ALPHA, true, "p2-strobe-warm-0");
    for i in 1..8 {
        run_accumulate(
            &h.device,
            &tracer,
            &scene_a,
            &depth_tex,
            &hi_normal,
            &mut history,
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
    run_accumulate(&h.device, &tracer, &flipped, &depth_tex, &hi_normal, &mut history, TEST_ALPHA, false, "p2-strobe-plus-one");
    let strobe_plus_one = readback_rgba_f32(history.current_irr());

    // Cold start: a FRESH history set seeing the flipped value for the
    // first time (reset=true) — what the strobed frame would look like
    // WITHOUT retained history.
    let mut cold_history = HistorySet::new(&h.device, "p2-strobe-cold-history");
    run_accumulate(&h.device, &tracer, &flipped, &depth_tex, &hi_normal, &mut cold_history, TEST_ALPHA, true, "p2-strobe-cold-start");
    let cold_start = readback_rgba_f32(cold_history.current_irr());

    let diff = mean_abs_diff_rgb(&strobe_plus_one, &cold_start);
    eprintln!("[P2] strobe+1-vs-cold-start mean abs diff = {diff}");
    assert!(
        diff > STROBE_RETAIN_EPSILON,
        "strobe+1 frame matches a cold start too closely (mean abs diff {diff} <= {STROBE_RETAIN_EPSILON}) — history was NOT retained; a light-intensity flip is being treated as a cut"
    );
}

/// RT-T2-C (object motion): the per-object reprojection keeps a MOVING
/// object's history where camera-only reprojection rejects it.
///
/// Fixture: identity camera both frames; the "object" (id 0, the value
/// `make_constant_normal`'s `.w` already carries) sits at NDC z 0.7 on the
/// history frame, then moves to z 0.5. `obj_motion[0]` is the matching
/// world→prev-world delta (translate z by +0.2).
///
/// - WITH the motion table (`obj_count = 1`): the reprojected history
///   depth (0.5 + 0.2 = 0.7) matches the stored 0.7 exactly → validity
///   passes → output is `mix(history, current, alpha)`. CPU-expected red
///   channel: `(1 - alpha) * 1.0`.
/// - WITHOUT it (`obj_count = 0`, camera-only — the pre-T2-C behavior):
///   0.5 vs stored 0.7 fails the 5e-3 depth reject → history discarded →
///   output is the current frame alone (red 0.0). This control leg is what
///   makes the first leg a proof of the OBJECT term specifically, not of
///   accumulation in general.
#[test]
fn object_motion_reprojection_retains_history_where_camera_only_rejects() {
    let h = shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    let irr_a = upload_irr(&h.device, 1.0, 0.0, 0.0, "t2c-irr-a");
    let irr_b = upload_irr(&h.device, 0.0, 0.0, 0.0, "t2c-irr-b");
    let depth_far = make_depth_at(&h.device, 0.7, "t2c-depth-far");
    let depth_near = make_depth_at(&h.device, 0.5, "t2c-depth-near");
    let hi_normal = make_constant_normal(&h.device, "t2c-normal");

    // Column-major translate-z(+0.2): world→prev-world for an object that
    // moved 0.2 TOWARD the camera this frame.
    let delta_z = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.2, 1.0],
    ];

    // Leg 1: object motion supplied — history must survive the move.
    let mut history = HistorySet::new(&h.device, "t2c-history");
    run_accumulate_with_motion(
        &h.device, &tracer, &irr_a, &depth_far, &hi_normal, &mut history, TEST_ALPHA, true, 1,
        IDENTITY, IDENTITY, "t2c-warm",
    );
    run_accumulate_with_motion(
        &h.device, &tracer, &irr_b, &depth_near, &hi_normal, &mut history, TEST_ALPHA, false, 1,
        delta_z, IDENTITY, "t2c-moved",
    );
    let with_motion = readback_rgba_f32(history.current_irr());

    // Leg 2 (control): identical frames, motion table absent — the depth
    // mismatch must reject history (pre-T2-C behavior).
    let mut control = HistorySet::new(&h.device, "t2c-control-history");
    run_accumulate_with_motion(
        &h.device, &tracer, &irr_a, &depth_far, &hi_normal, &mut control, TEST_ALPHA, true, 0,
        IDENTITY, IDENTITY, "t2c-control-warm",
    );
    run_accumulate_with_motion(
        &h.device, &tracer, &irr_b, &depth_near, &hi_normal, &mut control, TEST_ALPHA, false, 0,
        IDENTITY, IDENTITY, "t2c-control-moved",
    );
    let camera_only = readback_rgba_f32(control.current_irr());

    let expected_retained = 1.0 - SECOND_FRAME_ALPHA;
    let mean_r = |px: &[f32]| {
        px.iter().step_by(4).sum::<f32>() / (px.len() / 4) as f32
    };
    let with_r = mean_r(&with_motion);
    let control_r = mean_r(&camera_only);
    eprintln!("[T2-C] with-motion mean r = {with_r} (expect ~{expected_retained}), camera-only mean r = {control_r} (expect ~0.0)");
    assert!(
        (with_r - expected_retained).abs() < RESET_EPSILON,
        "object-motion reprojection did NOT retain the moved object's history (mean r {with_r}, expected {expected_retained})"
    );
    assert!(
        control_r < RESET_EPSILON,
        "camera-only control leg unexpectedly retained history across the depth move (mean r {control_r}) — the depth reject stopped discriminating and this proof can't isolate the object term"
    );
}

/// Upload helper for Rgba16Float with caller-controlled alpha — same
/// `CPU_UPLOAD | SHADER_READ` usage as `upload_irr` but preserving the
/// `.a` channel (irr alpha is unused so `upload_irr` writes 0.0; refl's
/// `.a` carries hit distance).
fn make_upload_rgba_f16(device: &GpuDevice, r: f32, g: f32, b: f32, a: f32, label: &str) -> GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width: W,
        height: H,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
        label,
        mip_levels: 1,
    });
    device.upload_texture(&texture, as_bytes(&flat_rgba_f16_with_a(W, H, r, g, b, a)));
    texture
}

/// BUG-dx6w: 2x2-tiled checkerboard upload — texel `(x,y)` gets `a_rgba` when
/// `(x+y)%2==0`, else `b_rgba`. Gives `clamp_refl_history`'s current-frame
/// neighborhood box nonzero variance (a constant-fill texture collapses
/// `sigma` to 0, degenerate for exercising the clamp itself).
fn make_upload_rgba_f16_checkerboard(
    device: &GpuDevice,
    a_rgba: [f32; 4],
    b_rgba: [f32; 4],
    label: &str,
) -> GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width: W,
        height: H,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
        label,
        mip_levels: 1,
    });
    let mut pixels = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let rgba = if (x + y) % 2 == 0 { a_rgba } else { b_rgba };
            for c in rgba {
                pixels.push(f16::from_f32(c));
            }
        }
    }
    device.upload_texture(&texture, as_bytes(&pixels));
    texture
}

/// Upload helper for a single-channel R32Float texture (depth side channel).
fn make_upload_r32(device: &GpuDevice, v: f32, label: &str) -> GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width: W,
        height: H,
        depth: 1,
        format: GpuTextureFormat::R32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
        label,
        mip_levels: 1,
    });
    let pixels = vec![v; (W * H) as usize];
    device.upload_texture(&texture, as_bytes(&pixels));
    texture
}

/// RT-R2 bisection instrument (lead-requested): does the refl channel's
/// accumulate kernel block blend at all? Seeds a known refl history H,
/// drives hi_refl at a checkerboard C, and measures the output at
/// reset=false (must variance-clip H toward C's neighborhood, then blend
/// toward C at the read pixel) and reset=true (must equal raw C at the read
/// pixel exactly). The lead reads the raw numbers — no diagnosis from this
/// file.
///
/// Normal history and depth history are seeded to match the current frame's
/// values (both constant), and reprojection uses identity matrices, so every
/// pixel's reprojection validity test passes unconditionally (self-
/// reprojection, weight 1 on the own tap — same discipline the module doc's
/// other proofs rely on).
///
/// BUG-dx6w: `accumulate_irradiance`'s refl blend now runs the seeded/
/// reprojected history through `clamp_refl_history` (variance clip to the
/// CURRENT frame's 3x3 `hi_refl` neighborhood, gamma = `RT_REFL_CLAMP_GAMMA`
/// = 1.0 in `crates/manifold-gpu/src/metal/raytrace.rs`) before the 0.9/0.1
/// blend — a CONSTANT hi_refl fill collapses that neighborhood's sigma to 0
/// and degenerates the clamp (every history value not exactly at the
/// constant collapses to it), so `hi_refl` here is a 2x2 checkerboard: this
/// gives the box nonzero variance and keeps the blend leg's power to prove
/// the kernel reads seeded history (a broken blend path can't reproduce this
/// specific clamp+blend number by accident). Readback samples pixel (0,0);
/// edge-clamped 3x3 footprint at that corner reads 4 taps at (0,0), 2 at
/// (1,0), 2 at (0,1), 1 at (1,1) (Metal `clamp` on the neighborhood index,
/// same as `clamp_refl_history`'s own edge handling).
///
/// BUG-axe9: the box itself is now built in Reinhard-mapped (Karis) space
/// (`t(c) = c / (1 + luma(c))`, inverted with `c = t / (1 - luma(t))`) — the
/// blend-leg expectation below maps the 9 neighborhood taps and the seeded
/// history, clamps in mapped space, then unmaps before the 0.9/0.1 blend,
/// mirroring `clamp_refl_history` exactly (`luma()` coefficients copied
/// from `crates/manifold-gpu/src/metal/raytrace.rs`).
#[test]
fn refl_channel_blends_history_and_current() {
    let h = shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);

    // ── Shared per-frame textures ─────────────────────────────────────
    let depth_tex = make_constant_depth(device, "bisect-depth");
    let hi_normal = make_constant_normal(device, "bisect-normal");
    let hi_irr = upload_irr(device, 0.0, 0.0, 0.0, "bisect-hi-irr");

    // Depth history seeded at the same 0.5 as depth_tex — identity
    // reprojection preserves depth exactly, passes the validity gate.
    let depth_history = make_upload_r32(device, 0.5, "bisect-depth-history");
    let depth_output =
        make_history_side_channel(device, GpuTextureFormat::R32Float, "bisect-depth-output");

    // Normal history seeded at +Y — matches hi_normal dot > 0.9.
    // `.w` = the specular channel's history length. 9 prior frames means the
    // kernel's `1/n` blend weight lands on exactly 1/10 = 0.1, the weight
    // every expectation below is computed against.
    let normal_history = make_upload_rgba_f16(device, 0.0, 1.0, 0.0, 9.0, "bisect-normal-history");
    let normal_output =
        make_history_side_channel(device, GpuTextureFormat::Rgba16Float, "bisect-normal-output");

    // Irradiance channel — valid bindings only.
    let irr_history = make_history(device, "bisect-irr-history");
    let irr_output = make_history(device, "bisect-irr-output");

    // Moments channel — valid bindings only (Rgba32Float since ED2: `.b`
    // carries the accumulated ao).
    let moments_history =
        make_history_side_channel(device, GpuTextureFormat::Rgba32Float, "bisect-moments-history");
    let moments_output =
        make_history_side_channel(device, GpuTextureFormat::Rgba32Float, "bisect-moments-output");

    // ── Reflection channel ────────────────────────────────────────────
    // H = (1.0, 0.0, 0.0, 5.0): history seed, .a = 5.0 is a valid hit distance.
    let refl_history = make_upload_rgba_f16(device, 1.0, 0.0, 0.0, 5.0, "bisect-refl-history");
    // BUG-dx6w: checkerboard current frame — A=(2,0,0,5) at (x+y) even,
    // B=(4,0,0,5) at (x+y) odd. Mean r=3.0 (same nominal "current" value the
    // pre-clamp test used), but nonzero neighborhood variance so the clamp
    // has a real box to clip H into instead of degenerating.
    const REFL_A: [f32; 4] = [2.0, 0.0, 0.0, 5.0];
    const REFL_B: [f32; 4] = [4.0, 0.0, 0.0, 5.0];
    let hi_refl = make_upload_rgba_f16_checkerboard(device, REFL_A, REFL_B, "bisect-hi-refl");
    let refl_output = make_history(device, "bisect-refl-output");

    // ── Buffers ───────────────────────────────────────────────────────
    let params_buffer =
        device.create_buffer_shared(std::mem::size_of::<AccumulateParams>() as u64);
    let obj_motion_buffer =
        device.create_buffer_shared(std::mem::size_of::<[[f32; 4]; 4]>() as u64);
    let gi_materials_buf = device.create_buffer_shared(std::mem::size_of::<GiMaterial>() as u64);
    // SV-ACCUM: inert fully-lit visibility input + history/output pair —
    // these legs assert on the refl channel only.
    let hi_sv = upload_irr(device, 1.0, 1.0, 1.0, "bisect-hi-sv");
    let sv_history = make_history(device, "bisect-sv-history");
    let sv_output = make_history(device, "bisect-sv-output");
    let sv_m1_in = make_history(device, "bisect-sv-m1-in");
    let sv_m1_out = make_history(device, "bisect-sv-m1-out");
    let sv_m2_in = make_history(device, "bisect-sv-m2-in");
    let sv_m2_out = make_history(device, "bisect-sv-m2-out");
    let sv_hold_in = make_history(device, "bisect-sv-hold-in");
    let sv_hold_out = make_history(device, "bisect-sv-hold-out");

    // ── Leg 1: reset = false — history must blend toward current ──────
    let blend_params = AccumulateParams::new([W, H], 0.1, false, 0, [0.0; 3], IDENTITY, IDENTITY);
    {
        let mut enc = device.create_encoder("bisect-blend");
        let gpu = RendererGpuEncoder::new(&mut enc, device);
        tracer.accumulate_irradiance(
            gpu.native_enc,
            &blend_params,
            &params_buffer,
            &obj_motion_buffer,
            &hi_irr,
            &depth_tex,
            &hi_normal,
            &irr_history,
            &irr_output,
            &depth_history,
            &depth_output,
            &normal_history,
            &normal_output,
            &moments_history,
            &moments_output,
            &hi_refl,
            &refl_history,
            &refl_output,
            &gi_materials_buf,
            &hi_sv,
            &sv_history,
            &sv_output,
            &sv_m1_in,
            &sv_m1_out,
            &sv_m2_in,
            &sv_m2_out,
            &sv_hold_in,
            &sv_hold_out,
            "bisect-blend",
        );
        enc.commit_and_wait_completed();
    }
    let blend = readback_rgba_f32(&refl_output);
    let blend_r = blend[0];
    let blend_g = blend[1];
    let blend_b = blend[2];
    let blend_a = blend[3];

    // BUG-dx6w: the readback pixel is (0,0), a texture corner — the MSL
    // `clamp_refl_history` edge-clamps its 3x3 neighborhood index the same
    // way, so the 9 taps collapse onto 4 distinct texels with the
    // multiplicities below (see the fn-level doc comment).
    //
    // BUG-axe9: the box is built in Reinhard-mapped space now (`t(c) = c /
    // (1 + luma(c))`, unmapped with `c = t / (1 - luma(t))`) — mirrors
    // `clamp_refl_history`'s exact math and `luma()`'s exact coefficients
    // (`crates/manifold-gpu/src/metal/raytrace.rs`). REFL_A/REFL_B only
    // populate the R channel, so luma isn't just R — the full 3-vector
    // form is used, not a scalar shortcut.
    fn luma(c: [f32; 3]) -> f32 {
        0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
    }
    fn tonemap(c: [f32; 3]) -> [f32; 3] {
        let l = luma(c);
        c.map(|x| x / (1.0 + l))
    }
    fn untonemap(t: [f32; 3]) -> [f32; 3] {
        let l = luma(t).min(0.999);
        let denom = 1.0 - l;
        t.map(|x| x / denom)
    }

    let history_seed = [1.0_f32, 0.0, 0.0]; // H = (1,0,0,5), rgb only
    let a3 = [REFL_A[0], REFL_A[1], REFL_A[2]];
    let b3 = [REFL_B[0], REFL_B[1], REFL_B[2]];
    let neighborhood: [[f32; 3]; 9] = [
        a3, a3, a3, a3, // (0,0) x4
        b3, b3,         // (1,0) x2
        b3, b3,         // (0,1) x2
        a3,             // (1,1) x1
    ];
    let mapped: Vec<[f32; 3]> = neighborhood.iter().map(|&c| tonemap(c)).collect();
    let n = mapped.len() as f32;
    let m1: [f32; 3] = std::array::from_fn(|i| mapped.iter().map(|c| c[i]).sum::<f32>() / n);
    let m2: [f32; 3] = std::array::from_fn(|i| mapped.iter().map(|c| c[i] * c[i]).sum::<f32>() / n);
    let sigma: [f32; 3] = std::array::from_fn(|i| (m2[i] - m1[i] * m1[i]).max(0.0).sqrt());
    // Mirrors RT_REFL_CLAMP_GAMMA (crates/manifold-gpu/src/metal/raytrace.rs) —
    // retuning that constant requires recomputing this expectation.
    const GAMMA: f32 = 1.0;
    let lo: [f32; 3] = std::array::from_fn(|i| m1[i] - GAMMA * sigma[i]);
    let hi: [f32; 3] = std::array::from_fn(|i| m1[i] + GAMMA * sigma[i]);
    let mapped_history = tonemap(history_seed);
    let clamped_mapped: [f32; 3] = std::array::from_fn(|i| mapped_history[i].clamp(lo[i], hi[i]));
    let clamped_h = untonemap(clamped_mapped)[0];
    let current_r_at_pixel = REFL_A[0]; // pixel (0,0): (0+0)%2==0 -> A
    let expected_r = 0.9 * clamped_h + 0.1 * current_r_at_pixel;

    // Sanity: the clamp+blend result must stay discriminating — distinct
    // from both the raw current value and the pre-clamp unclamped-blend
    // value, so this leg still proves the kernel reads seeded history
    // rather than degenerating to either endpoint.
    let history_seed_r = history_seed[0];
    let unclamped_blend_r = 0.9 * history_seed_r + 0.1 * current_r_at_pixel;
    assert!(
        (expected_r - current_r_at_pixel).abs() > 0.05,
        "test fixture bug: expected_r={expected_r} too close to raw current={current_r_at_pixel}"
    );
    assert!(
        (expected_r - unclamped_blend_r).abs() > 0.05,
        "test fixture bug: expected_r={expected_r} too close to the pre-clamp \
         unclamped blend value={unclamped_blend_r}"
    );

    eprintln!(
        "[bisect] refl blend leg (reset=false): \
         r={blend_r} (expect ~{expected_r} = mix(untonemap(clamp(tonemap({history_seed_r}), \
         {lo:?}, {hi:?})), {current_r_at_pixel}, 0.1); mapped m1={m1:?} sigma={sigma:?}), \
         g={blend_g}, b={blend_b}, a={blend_a}"
    );

    assert!(
        (blend_r - expected_r).abs() < 0.01,
        "refl channel with reset=false did NOT blend+clamp as expected: r={blend_r}, \
         expected ~{expected_r} (kernel defect: the refl blend term never reads seeded \
         history, or the mapped-space variance clip (BUG-axe9) isn't engaging)"
    );

    // ── Leg 2: reset = true — output must equal C exactly ─────────────
    let refl_output_reset = make_history(device, "bisect-refl-output-reset");
    let reset_params = AccumulateParams::new([W, H], 0.1, true, 0, [0.0; 3], IDENTITY, IDENTITY);
    {
        let mut enc = device.create_encoder("bisect-reset");
        let gpu = RendererGpuEncoder::new(&mut enc, device);
        tracer.accumulate_irradiance(
            gpu.native_enc,
            &reset_params,
            &params_buffer,
            &obj_motion_buffer,
            &hi_irr,
            &depth_tex,
            &hi_normal,
            &irr_history,
            &irr_output,
            &depth_history,
            &depth_output,
            &normal_history,
            &normal_output,
            &moments_history,
            &moments_output,
            &hi_refl,
            &refl_history,
            &refl_output_reset,
            &gi_materials_buf,
            &hi_sv,
            &sv_history,
            &sv_output,
            &sv_m1_in,
            &sv_m1_out,
            &sv_m2_in,
            &sv_m2_out,
            &sv_hold_in,
            &sv_hold_out,
            "bisect-reset",
        );
        enc.commit_and_wait_completed();
    }
    let reset = readback_rgba_f32(&refl_output_reset);
    let reset_r = reset[0];
    let reset_g = reset[1];
    let reset_b = reset[2];
    let reset_a = reset[3];
    eprintln!(
        "[bisect] refl reset leg (reset=true): \
         r={reset_r} (expect {current_r_at_pixel} = raw checkerboard C at pixel (0,0), \
         parity even -> A=(2,0,0,5)), \
         g={reset_g}, b={reset_b}, a={reset_a}"
    );

    assert!(
        (reset_r - current_r_at_pixel).abs() < 0.01,
        "refl channel with reset=true did NOT write current frame directly: \
         r={reset_r}, expected {current_r_at_pixel}"
    );
}
/// BUG-ukg (camera-motion smear): per-tap validated bilinear resampling
/// under a FRACTIONAL camera reprojection — the discriminating proof.
///
/// Fixture: alternating 0/1 red columns warm the history; the second frame
/// renders black irradiance through a `prev_view_proj` that translates NDC
/// x by +0.0375 = 0.6 texel at W=32 (`inv_view_proj` stays IDENTITY), so a
/// texel at pixel x reprojects to fractional pixel x+0.6: footprint taps
/// {x, x+1} with weights {0.4, 0.6}; y lands integer, second-row weight 0.
///
/// Why this fails pre-fix: the old kernel read ONE nearest tap,
/// `int(x + 1.1) = x+1`, so an even column read h(x+1) = 1.0 and output
/// 0.85, not the validated bilinear 0.85 * (0.4*0 + 0.6*1) = 0.51.
///
/// Rejection leg: the warm frame's depth is 0.5 everywhere except texel
/// (11,16) at 0.9, so frame 2 (uniform depth 0.5) finds that tap's stored
/// depth invalid at pixel (10,16) — zero weight, renormalized to h(10)
/// alone = 0.0. A plain UNVALIDATED bilinear would output the same 0.51 as
/// the blend leg — this leg pins the per-tap validation itself.
#[test]
fn fractional_camera_reprojection_blends_and_rejects_per_tap() {
    let h = shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    // prev_view_proj: translate NDC x by +0.0375 (0.6 texel at W=32).
    // Column-major like `rot_z` below: column 3 is translation.
    let frac_shift = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0375, 0.0, 0.0, 1.0],
    ];

    // Alternating red columns (exact in f16): h(x) = x % 2.
    let irr_hist = {
        let mut px = Vec::with_capacity((W * H * 4) as usize);
        for _y in 0..H {
            for x in 0..W {
                px.push(f16::from_f32((x % 2) as f32));
                px.push(f16::from_f32(0.0));
                px.push(f16::from_f32(0.0));
                px.push(f16::from_f32(0.0));
            }
        }
        let t = h.device.create_texture(&GpuTextureDesc {
            width: W,
            height: H,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "ukg-irr-columns",
            mip_levels: 1,
        });
        h.device.upload_texture(&t, as_bytes(&px));
        t
    };
    let irr_black = upload_irr(&h.device, 0.0, 0.0, 0.0, "ukg-irr-black");

    // Warm depth: 0.5 everywhere, texel (11,16) corrupted to 0.9 — its
    // stored depth history fails frame 2's validity test at that tap.
    let depth_warm = {
        let mut px = vec![0.5f32; (W * H) as usize];
        px[(16 * W + 11) as usize] = 0.9;
        let t = h.device.create_texture(&GpuTextureDesc {
            width: W,
            height: H,
            depth: 1,
            format: GpuTextureFormat::Depth32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "ukg-depth-spot",
            mip_levels: 1,
        });
        h.device.upload_texture(&t, as_bytes(&px));
        t
    };
    let depth_flat = make_constant_depth(&h.device, "ukg-depth-flat");
    let normal = make_constant_normal(&h.device, "ukg-normal");

    let mut history = HistorySet::new(&h.device, "ukg-history");
    run_accumulate_with_motion(
        &h.device, &tracer, &irr_hist, &depth_warm, &normal, &mut history, TEST_ALPHA, true, 0,
        IDENTITY, IDENTITY, "ukg-warm",
    );
    run_accumulate_with_motion(
        &h.device, &tracer, &irr_black, &depth_flat, &normal, &mut history, TEST_ALPHA, false, 0,
        IDENTITY, frac_shift, "ukg-frac-shift",
    );
    let out = readback_rgba_f32(history.current_irr());
    let red = |x: u32, y: u32| out[((y * W + x) * 4) as usize];

    // Bilinear leg, away from the corrupted texel: even column 20, row 8.
    let expected_blend = (1.0 - SECOND_FRAME_ALPHA) * 0.6;
    let got_blend = red(20, 8);
    eprintln!("[BUG-ukg] bilinear leg r = {got_blend} (expect {expected_blend})");
    assert!(
        (got_blend - expected_blend).abs() < RESET_EPSILON,
        "BUG-ukg: fractional camera reprojection is not a validated bilinear \
         blend (r {got_blend}, expected {expected_blend}; pre-fix nearest-tap \
         behavior reads h(x+1) alone = 0.85)"
    );

    // Rejection leg: pixel (10,16)'s footprint tap (11,16) carries the
    // corrupted stored depth — zero weight, renormalized to h(10) = 0.0.
    let got_reject = red(10, 16);
    eprintln!("[BUG-ukg] rejection leg r = {got_reject} (expect 0.0)");
    assert!(
        got_reject.abs() < RESET_EPSILON,
        "BUG-ukg: per-tap validation failed to zero-weight an invalid tap \
         (r {got_reject}, expected 0.0 — the corrupted neighbor's history \
         leaked into the blend)"
    );
}

/// BUG-322: a ROTATING object must keep its temporal history.
///
/// T2-C carried the reprojected world position through the object's motion
/// but compared normals raw. `normal_history` holds WORLD-space normals, so
/// on a rotating object the stored normal is in last frame's orientation
/// and `cur_normal` is in this frame's — they disagree by exactly the
/// rotation, the validity test rejects, and the surface falls back to raw
/// per-frame sample counts for the whole gesture. That is the shimmer Peter
/// saw on the DamagedHelmet (curved + normal-mapped, so the disagreement is
/// large per pixel) while flat flowers looked fine. A translation-only
/// oracle cannot see this: translation leaves normals untouched.
///
/// Fixture: the object rotates 35 degrees about +Z between frames. Depth is
/// held constant and `obj_motion`'s translation is identity, so the depth
/// half of the validity test passes either way and the normal term alone
/// decides the outcome.
///
/// - Correct (normals carried into one orientation): the two normals are
///   identical, `dot` = 1.0, validity passes — CPU-expected red `1 - alpha`.
/// - Pre-fix (raw comparison): the normals sit exactly the rotation apart,
///   `dot` = `cos(35 deg)` = 0.819, below the 0.9 threshold — history
///   discarded, red 0.0.
///
/// **Honest scope of this fixture.** 35 degrees is chosen because the pure
/// rotation term only crosses the 0.9 (~26 degree) threshold above that, so
/// a smaller angle would pass pre-fix and prove nothing. Rotation alone is
/// therefore not the whole story for the helmet at ordinary drag speeds —
/// what makes it bite far sooner there is that a rotating CURVED,
/// normal-mapped surface also lands each reprojection on a different
/// surface point whose normal differs by much more than the object's own
/// rotation angle, on top of this systematic orientation error. This test
/// pins the invariant that is unambiguously wrong and fixable: the two
/// normals must be compared in ONE orientation.
#[test]
fn rotating_object_retains_history_when_normals_are_compared_in_one_orientation() {
    let h = shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    const ROT: f32 = std::f32::consts::PI * 35.0 / 180.0;
    // Column-major rotation about +Z by ROT — the object's world->prev-world
    // delta (it rotated by -ROT this frame, so carrying a current normal
    // back to the previous frame rotates it by +ROT).
    let (s, c) = ROT.sin_cos();
    let rot_z = [
        [c, s, 0.0, 0.0],
        [-s, c, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    // History frame: surface normal is +Y (what `make_constant_normal`
    // writes), object id 0 in `.w`.
    let irr_a = upload_irr(&h.device, 1.0, 0.0, 0.0, "t322-irr-a");
    let irr_b = upload_irr(&h.device, 0.0, 0.0, 0.0, "t322-irr-b");
    let depth = make_constant_depth(&h.device, "t322-depth");
    let normal_prev = make_object_normal(&h.device, [0.0, 1.0, 0.0], "t322-n-prev");
    // This frame the object has rotated by -ROT about Z, so the SAME
    // surface point's world normal is +Y rotated by -ROT.
    let cur_n = [ROT.sin(), ROT.cos(), 0.0];
    let normal_cur = make_object_normal(&h.device, cur_n, "t322-n-cur");

    let mut history = HistorySet::new(&h.device, "t322-history");
    run_accumulate_with_motion(
        &h.device, &tracer, &irr_a, &depth, &normal_prev, &mut history, TEST_ALPHA, true, 1,
        IDENTITY, IDENTITY, "t322-warm",
    );
    run_accumulate_with_motion(
        &h.device, &tracer, &irr_b, &depth, &normal_cur, &mut history, TEST_ALPHA, false, 1,
        rot_z, IDENTITY, "t322-rotated",
    );
    let out = readback_rgba_f32(history.current_irr());

    // Interior only. `obj_motion` rotates world POSITIONS about the origin
    // too, so texels near the border reproject off-screen and are correctly
    // rejected as disocclusion — real behavior, not the defect under test.
    // The invariant here ("same surface point, same orientation => history
    // retained") is only defined where the reprojection lands on-screen, so
    // the measurement is the central half rather than a loosened threshold.
    let expected = 1.0 - SECOND_FRAME_ALPHA;
    let (lo, hi) = (W / 4, W - W / 4);
    let mut acc = 0.0f32;
    let mut n = 0u32;
    for y in lo..hi {
        for x in lo..hi {
            acc += out[((y * W + x) * 4) as usize];
            n += 1;
        }
    }
    let mean_r = acc / n as f32;
    eprintln!("[BUG-322] rotating-object mean r = {mean_r} (expect ~{expected} = history retained)");
    assert!(
        (mean_r - expected).abs() < RESET_EPSILON,
        "BUG-322: a rotating object lost its temporal history (mean r {mean_r}, expected \
         {expected}). The normal validity test is comparing a stored world-space normal from the \
         previous orientation against this frame's normal without carrying one into the other's \
         frame, so it rejects by exactly the object's rotation — raw sample counts for the whole \
         gesture, i.e. the helmet shimmer."
    );
}

/// The same cue at LOW signal magnitude — the case an absolute floor kills.
///
/// The first version of the change gate used `max(sigmas * sd, rel * luma,
/// 0.01)`. That 0.01 is a perceptual floor living in a signal space whose scale
/// is scene-dependent: real scenes measure demodulated irradiance around 5e-4,
/// three orders under it, so `max` pinned the gate at 0.01 and nothing could
/// ever trip. The gate was dead in exactly the scenes that needed it, and Peter
/// found it by dropping sun intensity 10 -> 0 and watching the object still
/// fade. This test is the regression guard for that whole class: same fixture
/// as the cue proof above, scaled down 50x.
///
/// Scene A red 0.02 (luma 4.25e-3) -> B red 0.004 (luma 8.5e-4). |delta| is
/// 3.4e-3, which clears the relative gate (0.15 * 4.25e-3 = 6.4e-4) but sits
/// well UNDER an 0.01 absolute floor. Correct snaps to 0.012; the floored
/// version would read ~0.0177.
#[test]
fn lighting_cue_snaps_at_low_signal_magnitude_too() {
    let h = shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    let depth_tex = make_constant_depth(&h.device, "dim-cue-depth");
    let hi_normal = make_constant_normal(&h.device, "dim-cue-normal");
    let scene_a = upload_irr(&h.device, 0.02, 0.0, 0.0, "dim-cue-a");
    let scene_b = upload_irr(&h.device, 0.004, 0.0, 0.0, "dim-cue-b");

    let mut history = HistorySet::new(&h.device, "dim-cue-history");
    run_accumulate(
        &h.device, &tracer, &scene_a, &depth_tex, &hi_normal, &mut history, TEST_ALPHA, true,
        "dim-cue-warm-0",
    );
    for i in 1..6 {
        run_accumulate(
            &h.device, &tracer, &scene_a, &depth_tex, &hi_normal, &mut history, TEST_ALPHA, false,
            &format!("dim-cue-warm-{i}"),
        );
    }
    run_accumulate(
        &h.device, &tracer, &scene_b, &depth_tex, &hi_normal, &mut history, TEST_ALPHA, false,
        "dim-cue-step",
    );

    let out = readback_rgba_f32(history.current_irr());
    let got = out[(((H / 2) * W + W / 2) * 4) as usize];
    let expected_snap = 0.012_f32; // mix(0.02, 0.004, 0.5)
    let expected_floored = 0.02 - 0.016 / 7.0; // ~0.0177, the dead-gate reading
    eprintln!(
        "[dim cue] r = {got} (snap expects {expected_snap}, dead-gate would read \
         {expected_floored})"
    );
    assert!(
        (got - expected_snap).abs() < 0.0015,
        "a lighting cue at low signal magnitude did not snap (r {got}, expected \
         {expected_snap}; {expected_floored} is the behaviour when an absolute perceptual floor \
         pins the gate above the whole signal range — the fade Peter saw dropping sun 10 -> 0)"
    );
}

/// A hard lighting cue must land in ~one frame, not average in over the
/// accumulator's whole window.
///
/// The static-boil work stretched the temporal window to 40-50 frames, and
/// Peter immediately hit the consequence on stage: automated light moves felt
/// slow and hard transitions stopped landing. Averaging and responsiveness only
/// conflict while you cannot tell noise from signal — the moments texture
/// already tracks per-texel luma spread, so the accumulator snaps when this
/// frame sits further from history than that spread can explain.
///
/// Fixture: warm a texel on scene A until its 1/n weight is small (n = 6, so
/// 1/7 next frame), then present scene B whose luma differs far more than the
/// gate. Constant fixtures leave the tracked spread at ~0, so the gate is
/// `max(0.15 * hist_luma, 0.01)`.
///
/// - Correct: gate trips, count collapses to 2, weight 0.5 — the output lands
///   halfway to B in ONE frame.
/// - Pre-fix: weight stays 1/7 and the output barely moves off A. That is the lag.
///
/// The two expectations are far apart (0.6 vs 0.886), so this cannot pass by
/// accident.
#[test]
fn hard_lighting_change_snaps_instead_of_averaging_in() {
    let h = shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    let depth_tex = make_constant_depth(&h.device, "cue-depth");
    let hi_normal = make_constant_normal(&h.device, "cue-normal");
    // A: red 1.0 (luma 0.2126). B: red 0.2 (luma 0.0425). |delta| = 0.170, far
    // above the gate at max(0.15 * 0.2126, 0.01) = 0.0319.
    let scene_a = upload_irr(&h.device, 1.0, 0.0, 0.0, "cue-a");
    let scene_b = upload_irr(&h.device, 0.2, 0.0, 0.0, "cue-b");

    let mut history = HistorySet::new(&h.device, "cue-history");
    run_accumulate(
        &h.device, &tracer, &scene_a, &depth_tex, &hi_normal, &mut history, TEST_ALPHA, true,
        "cue-warm-0",
    );
    for i in 1..6 {
        run_accumulate(
            &h.device, &tracer, &scene_a, &depth_tex, &hi_normal, &mut history, TEST_ALPHA, false,
            &format!("cue-warm-{i}"),
        );
    }
    run_accumulate(
        &h.device, &tracer, &scene_b, &depth_tex, &hi_normal, &mut history, TEST_ALPHA, false,
        "cue-step",
    );

    let out = readback_rgba_f32(history.current_irr());
    let got = out[(((H / 2) * W + W / 2) * 4) as usize];
    // Snap: mix(1.0, 0.2, 0.5) = 0.6. Lag at 1/7: mix(1.0, 0.2, 0.1429) = 0.886.
    let expected_snap = 0.6_f32;
    let expected_lag = 1.0 - 0.8 / 7.0;
    eprintln!(
        "[cue] r = {got} (snap expects {expected_snap}, pre-fix lag would read {expected_lag})"
    );
    assert!(
        (got - expected_snap).abs() < 0.02,
        "a hard lighting change did not land in one frame (r {got}, expected {expected_snap}; \
         {expected_lag} is the pre-fix behaviour where a cue averages in over the accumulator's \
         whole window — the on-stage symptom Peter reported as lights lagging)"
    );
}

// ── SV-ACCUM proofs (2026-07-31) ─────────────────────────────────────
// The shadow-visibility channel was the only RT channel with no temporal
// accumulation: raw per-frame half-res samples straight to the fragment
// shader, the penumbra boil Peter reported. These proofs pin the
// channel's new contract: it converges temporally like every other
// channel (a max-amplitude flicker input collapses to its mean with a
// bounded residual), a reset still cold-starts it, and a real shadow
// arrival snaps the blend until the new level converges.

#[test]
fn sv_channel_converges_under_max_amplitude_flicker() {
    let h = shared();
    let tracer = MetalShadowRayTracer::new(&h.device);
    let depth_tex = make_constant_depth(&h.device, "sv-accum-depth");
    let hi_normal = make_constant_normal(&h.device, "sv-accum-normal");
    // Constant irradiance so the irr change gate never trips (it keys on
    // irr luma) — the sv blend runs at 1/n undisturbed, which is exactly
    // the convergence behavior under test.
    let hi_irr = upload_irr(&h.device, 0.5, 0.5, 0.5, "sv-accum-irr");
    // Penumbra BOIL, not a square wave: 0.3/0.7 alternation around the 0.5
    // mean — amplitude 0.4. Once the moments converge the per-channel sigma
    // sits at ~0.2 and the 4-sigma gate band (~0.8) never trips, so the
    // blend amortizes at 1/n; a full 0↔1 step is a shadow edge crossing
    // and correctly snaps. Constant irradiance keeps the irr gate quiet
    // for the same reason.
    let sv_lo = upload_irr(&h.device, 0.3, 0.3, 0.3, "sv-accum-sv-lo");
    let sv_hi = upload_irr(&h.device, 0.7, 0.7, 0.7, "sv-accum-sv-hi");
    let mut history = HistorySet::new(&h.device, "sv-accum-history");

    // Alternate dim/bright frames around the mean. Center pixel read back
    // at frames 19 and 21.
    let mut v19 = 0.0f32;
    for frame in 0..22 {
        let sv = if frame % 2 == 0 { &sv_lo } else { &sv_hi };
        run_accumulate_with_sv(
            &h.device, &tracer, &hi_irr, &depth_tex, &hi_normal, &mut history,
            TEST_ALPHA, frame == 0, 0, [[0.0; 4]; 4], IDENTITY, sv,
            &format!("sv-accum-frame-{frame}"),
        );
        if frame == 19 {
            v19 = readback_rgba_f32(history.current_sv())[(((H / 2) * W + W / 2) * 4) as usize];
        }
    }
    let v21 = readback_rgba_f32(history.current_sv())[(((H / 2) * W + W / 2) * 4) as usize];

    let mean = (v19 + v21) / 2.0;
    let delta = (v21 - v19).abs();
    eprintln!("[sv-accum] converged mean = {mean} (expect ~0.5), late frame-to-frame delta = {delta} (raw input swings 0.4)");
    assert!(
        (mean - 0.5).abs() < 0.06,
        "sv channel did not converge to the input mean (mean {mean}, expected ~0.5) — \
         the running-mean blend is not engaging on the visibility channel"
    );
    assert!(
        delta < 0.08,
        "sv channel still boils late in a static scene (frame-to-frame delta {delta} at frame ~20, \
         raw input swings 0.4) — temporal amortization is not reaching the shadow mask"
    );
}

#[test]
fn sv_channel_reset_cold_starts_from_current_frame() {
    let h = shared();
    let tracer = MetalShadowRayTracer::new(&h.device);
    let depth_tex = make_constant_depth(&h.device, "sv-reset-depth");
    let hi_normal = make_constant_normal(&h.device, "sv-reset-normal");
    let hi_irr = upload_irr(&h.device, 0.5, 0.5, 0.5, "sv-reset-irr");
    // Warm the sv history on a lit frame, then reset on a 0.25 frame:
    // history must read exactly the current frame, no lit ghost.
    let sv_lit = upload_irr(&h.device, 1.0, 1.0, 1.0, "sv-reset-sv-lit");
    let sv_quarter = upload_irr(&h.device, 0.25, 0.25, 0.25, "sv-reset-sv-quarter");
    let mut history = HistorySet::new(&h.device, "sv-reset-history");
    for frame in 0..6 {
        run_accumulate_with_sv(
            &h.device, &tracer, &hi_irr, &depth_tex, &hi_normal, &mut history,
            TEST_ALPHA, frame == 0, 0, [[0.0; 4]; 4], IDENTITY, &sv_lit,
            &format!("sv-reset-warm-{frame}"),
        );
    }
    run_accumulate_with_sv(
        &h.device, &tracer, &hi_irr, &depth_tex, &hi_normal, &mut history,
        TEST_ALPHA, true, 0, [[0.0; 4]; 4], IDENTITY, &sv_quarter,
        "sv-reset-cut",
    );
    let got = readback_rgba_f32(history.current_sv())[(((H / 2) * W + W / 2) * 4) as usize];
    eprintln!("[sv-reset] post-reset sv = {got} (expect exactly 0.25)");
    assert!(
        (got - 0.25).abs() < 1e-3,
        "sv history kept a ghost through reset (read {got}, expected 0.25) — \
         a scene cut would leave stale shadows on the frame"
    );
}

#[test]
fn sv_channel_snaps_when_shadow_arrives() {
    // PROBE (SV-ACCUM finding 2 discrimination, 2026-07-31): the scene-level
    // rt_object_motion_shadow proof shows the destination shadow NEVER
    // forming after an occluder move — the whole ground stays lit. This is
    // the same scenario synthetically: warm the sv history fully lit, then
    // feed fully-shadowed frames WITHOUT reset; the sigma gate must trip
    // and the snap-hold must keep the blend at n=2 (alpha 0.5) until the
    // accumulated visibility converges — a gate that fires once and then
    // deadens on its own straddling moments decays at 1/n instead and the
    // shadow never forms within the proof's window.
    let h = shared();
    let tracer = MetalShadowRayTracer::new(&h.device);
    let depth_tex = make_constant_depth(&h.device, "sv-snap-depth");
    let hi_normal = make_constant_normal(&h.device, "sv-snap-normal");
    let hi_irr = upload_irr(&h.device, 0.5, 0.5, 0.5, "sv-snap-irr");
    let sv_lit = upload_irr(&h.device, 1.0, 1.0, 1.0, "sv-snap-sv-lit");
    let sv_dark = upload_irr(&h.device, 0.0, 0.0, 0.0, "sv-snap-sv-dark");
    let mut history = HistorySet::new(&h.device, "sv-snap-history");
    for frame in 0..6 {
        run_accumulate_with_sv(
            &h.device, &tracer, &hi_irr, &depth_tex, &hi_normal, &mut history,
            TEST_ALPHA, frame == 0, 0, [[0.0; 4]; 4], IDENTITY, &sv_lit,
            &format!("sv-snap-warm-{frame}"),
        );
    }
    let mut last = 1.0f32;
    for frame in 0..6 {
        run_accumulate_with_sv(
            &h.device, &tracer, &hi_irr, &depth_tex, &hi_normal, &mut history,
            TEST_ALPHA, false, 0, [[0.0; 4]; 4], IDENTITY, &sv_dark,
            &format!("sv-snap-dark-{frame}"),
        );
        last = readback_rgba_f32(history.current_sv())[(((H / 2) * W + W / 2) * 4) as usize];
        eprintln!("[sv-snap] after dark frame {frame}: sv = {last}");
    }
    assert!(
        last < 0.2,
        "sv history did not snap when a shadow arrived (still {last} after 6 fully-shadowed \
         frames) — the sigma gate or its snap-hold is not engaging"
    );
}
