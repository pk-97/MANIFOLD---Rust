//! `docs/RAYTRACING_DESIGN.md` section 10 addendum (gesture rule) — value-level
//! proofs for step-response latency, sub-threshold scrub tracking, and
//! per-channel sv/svt leg behavior under strobes and geometry moves.
//!
//! All tests exercise `accumulate_irradiance` directly via `MetalShadowRayTracer`
//! on a flat 32×32 fixture (constant depth/normal, identity view-proj — every
//! texel is self-reprojecting, same discipline as `rt_p2_soft_ao_temporal.rs`).
//!
//! ## Today's step-response number (pre-gesture kernel, measured analytically)
//!
//! With `lighting_changed=true` (n=2 snap) on a single-step change from a
//! converged scene: 4 frames to reach 90% of step magnitude (alpha 0.5 blend,
//! running-mean: 1→0.5, 2→0.75, 3→0.875, 4→0.9375). Without gesture, frame 2
//! (the frame after the snap frame) sees n=3 (alpha ≈ 0.33), softening the tail.
//! With gesture (n=2 held for the gesture's duration), the tail stays crisp:
//! the same 4-frame count but the per-frame progress is linear at alpha 0.5
//! (50→75→87.5→93.75% of step) instead of decaying.
//!
//! ## After-implementation target
//!
//! The gesture path holds n=2 for the full counter duration. A single-step
//! change with `lighting_changed=true` on frame 0 and `gesture=true` on frames
//! 1–2 reaches 90% within 3 frames from the step (93.75% at frame 3).

use std::mem::size_of;

use half::f16;
use manifold_gpu::raytrace::{AccumulateParams, GiMaterial, MetalShadowRayTracer, ShadowRayTracer};
use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;

use crate::harness::shared;

const W: u32 = 32;
const H: u32 = 32;

/// Blend-weight floor — matches `render_scene.rs`'s `IRRADIANCE_ACCUM_ALPHA`.
const TEST_ALPHA: f32 = 0.05;

/// Identity view-proj — self-reprojecting fixture (no real camera).
const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// Lighting-changed flag (bit 1 in `AccumulateParams::reset`).
const STEP_FLAG: u32 = manifold_gpu::raytrace::ACCUM_FLAG_LIGHTING_CHANGED;

/// Gesture-in-progress flag (bit 2).
const GESTURE_FLAG: u32 = manifold_gpu::raytrace::ACCUM_FLAG_GESTURE;

/// Geometry-changed flag (bit 3).
const GEO_CHANGED_FLAG: u32 = manifold_gpu::raytrace::ACCUM_FLAG_GEO_CHANGED;

/// Geometry-gesture flag (bit 4).
const GEO_GESTURE_FLAG: u32 = manifold_gpu::raytrace::ACCUM_FLAG_GEO_GESTURE;

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

fn upload_irr(device: &GpuDevice, r: f32, g: f32, b: f32) -> GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width: W, height: H, depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
        label: "gesture-irr", mip_levels: 1,
    });
    device.upload_texture(&texture, as_bytes(&flat_rgba_f16(W, H, r, g, b)));
    texture
}

fn make_history(device: &GpuDevice, label: &str) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: W, height: H, depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
        label, mip_levels: 1,
    })
}

fn make_history_side_channel(device: &GpuDevice, format: GpuTextureFormat, label: &str) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: W, height: H, depth: 1, format, dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::SHADER_READ,
        label, mip_levels: 1,
    })
}

fn make_pass_through(device: &GpuDevice, r: f32, g: f32, b: f32, label: &str) -> GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width: W, height: H, depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
        label, mip_levels: 1,
    });
    device.upload_texture(&texture, as_bytes(&flat_rgba_f16(W, H, r, g, b)));
    texture
}

fn make_depth_at(device: &GpuDevice, z: f32) -> GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width: W, height: H, depth: 1,
        format: GpuTextureFormat::Depth32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
        label: "gr-depth", mip_levels: 1,
    });
    let pixels = vec![z; (W * H) as usize];
    let bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), std::mem::size_of_val(&pixels[..]))
    };
    device.upload_texture(&texture, bytes);
    texture
}

/// Full history set for all channels the accumulate kernel needs.
struct FullHistorySet {
    irr: [GpuTexture; 2],
    depth: [GpuTexture; 2],
    normal: [GpuTexture; 2],
    moments: [GpuTexture; 2],
    refl: [GpuTexture; 2],
    sv: [GpuTexture; 2],
    sv_m1: [GpuTexture; 2],
    sv_m2: [GpuTexture; 2],
    sv_hold: [GpuTexture; 2],
    sv2: [GpuTexture; 2],
    sv2_m1: [GpuTexture; 2],
    sv2_m2: [GpuTexture; 2],
    sv2_hold: [GpuTexture; 2],
    svt: [GpuTexture; 2],
    ping: usize,
}

impl FullHistorySet {
    fn new(device: &GpuDevice) -> Self {
        Self {
            irr: [make_history(device, "gr-irr-a"), make_history(device, "gr-irr-b")],
            depth: [
                make_history_side_channel(device, GpuTextureFormat::R32Float, "gr-depth-a"),
                make_history_side_channel(device, GpuTextureFormat::R32Float, "gr-depth-b"),
            ],
            normal: [
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, "gr-normal-a"),
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, "gr-normal-b"),
            ],
            moments: [
                make_history_side_channel(device, GpuTextureFormat::Rgba32Float, "gr-moments-a"),
                make_history_side_channel(device, GpuTextureFormat::Rgba32Float, "gr-moments-b"),
            ],
            refl: [make_history(device, "gr-refl-a"), make_history(device, "gr-refl-b")],
            sv: [make_history(device, "gr-sv-a"), make_history(device, "gr-sv-b")],
            sv_m1: [
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, "gr-svm1-a"),
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, "gr-svm1-b"),
            ],
            sv_m2: [
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, "gr-svm2-a"),
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, "gr-svm2-b"),
            ],
            sv_hold: [
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, "gr-svh-a"),
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, "gr-svh-b"),
            ],
            sv2: [make_history(device, "gr-sv2-a"), make_history(device, "gr-sv2-b")],
            sv2_m1: [
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, "gr-sv2m1-a"),
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, "gr-sv2m1-b"),
            ],
            sv2_m2: [
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, "gr-sv2m2-a"),
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, "gr-sv2m2-b"),
            ],
            sv2_hold: [
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, "gr-sv2h-a"),
                make_history_side_channel(device, GpuTextureFormat::Rgba16Float, "gr-sv2h-b"),
            ],
            svt: [make_history(device, "gr-svt-a"), make_history(device, "gr-svt-b")],
            ping: 0,
        }
    }
    fn read_irr(&self) -> &GpuTexture { &self.irr[self.ping] }
    fn write_irr(&self) -> &GpuTexture { &self.irr[1 - self.ping] }
    fn read_depth(&self) -> &GpuTexture { &self.depth[self.ping] }
    fn write_depth(&self) -> &GpuTexture { &self.depth[1 - self.ping] }
    fn read_normal(&self) -> &GpuTexture { &self.normal[self.ping] }
    fn write_normal(&self) -> &GpuTexture { &self.normal[1 - self.ping] }
    fn read_moments(&self) -> &GpuTexture { &self.moments[self.ping] }
    fn write_moments(&self) -> &GpuTexture { &self.moments[1 - self.ping] }
    fn read_refl(&self) -> &GpuTexture { &self.refl[self.ping] }
    fn write_refl(&self) -> &GpuTexture { &self.refl[1 - self.ping] }
    fn read_sv(&self) -> &GpuTexture { &self.sv[self.ping] }
    fn write_sv(&self) -> &GpuTexture { &self.sv[1 - self.ping] }
    fn read_sv_m1(&self) -> &GpuTexture { &self.sv_m1[self.ping] }
    fn write_sv_m1(&self) -> &GpuTexture { &self.sv_m1[1 - self.ping] }
    fn read_sv_m2(&self) -> &GpuTexture { &self.sv_m2[self.ping] }
    fn write_sv_m2(&self) -> &GpuTexture { &self.sv_m2[1 - self.ping] }
    fn read_sv_hold(&self) -> &GpuTexture { &self.sv_hold[self.ping] }
    fn write_sv_hold(&self) -> &GpuTexture { &self.sv_hold[1 - self.ping] }
    fn read_sv2(&self) -> &GpuTexture { &self.sv2[self.ping] }
    fn write_sv2(&self) -> &GpuTexture { &self.sv2[1 - self.ping] }
    fn read_sv2_m1(&self) -> &GpuTexture { &self.sv2_m1[self.ping] }
    fn write_sv2_m1(&self) -> &GpuTexture { &self.sv2_m1[1 - self.ping] }
    fn read_sv2_m2(&self) -> &GpuTexture { &self.sv2_m2[self.ping] }
    fn write_sv2_m2(&self) -> &GpuTexture { &self.sv2_m2[1 - self.ping] }
    fn read_sv2_hold(&self) -> &GpuTexture { &self.sv2_hold[self.ping] }
    fn write_sv2_hold(&self) -> &GpuTexture { &self.sv2_hold[1 - self.ping] }
    fn read_svt(&self) -> &GpuTexture { &self.svt[self.ping] }
    fn write_svt(&self) -> &GpuTexture { &self.svt[1 - self.ping] }
    fn advance(&mut self) { self.ping = 1 - self.ping; }
}

/// Run one accumulate frame on a FullHistorySet. `reset_flags` ORs into
/// `AccumulateParams::reset` for lighting-changed/gesture/geo bits.
#[allow(clippy::too_many_arguments)]
fn run_accumulate_frame(
    device: &GpuDevice,
    tracer: &MetalShadowRayTracer,
    hi_irr: &GpuTexture,
    depth_tex: &GpuTexture,
    hi_normal: &GpuTexture,
    hi_sv: &GpuTexture,
    hi_sv2: &GpuTexture,
    hi_svt: &GpuTexture,
    hi_refl: &GpuTexture,
    history: &mut FullHistorySet,
    gi_materials_buf: &manifold_gpu::GpuBuffer,
    obj_motion_buf: &manifold_gpu::GpuBuffer,
    alpha: f32,
    reset: bool,
    reset_flags: u32,
    label: &str,
) {
    let params_buffer = device.create_buffer_shared(std::mem::size_of::<AccumulateParams>() as u64);
    let mut params = AccumulateParams::new(
        [W, H], alpha, reset, 1, [0.0; 3], 0.0, IDENTITY, IDENTITY,
    );
    params.reset |= reset_flags;
    let mut enc = device.create_encoder(label);
    {
        let gpu = RendererGpuEncoder::new(&mut enc, device);
        tracer.accumulate_irradiance(
            gpu.native_enc,
            &params,
            &params_buffer,
            obj_motion_buf,
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
            hi_refl,
            history.read_refl(),
            history.write_refl(),
            gi_materials_buf,
            hi_sv,
            history.read_sv(),
            history.write_sv(),
            history.read_sv_m1(),
            history.write_sv_m1(),
            history.read_sv_m2(),
            history.write_sv_m2(),
            history.read_sv_hold(),
            history.write_sv_hold(),
            hi_sv2,
            history.read_sv2(),
            history.write_sv2(),
            history.read_sv2_m1(),
            history.write_sv2_m1(),
            history.read_sv2_m2(),
            history.write_sv2_m2(),
            history.read_sv2_hold(),
            history.write_sv2_hold(),
            hi_svt,
            history.read_svt(),
            history.write_svt(),
            label,
        );
    }
    enc.commit_and_wait_completed();
    history.advance();
}

fn read_r_center(device: &GpuDevice, texture: &GpuTexture) -> f32 {
    let bytes_per_row = W * 8; // Rgba16Float = 8 bytes/px
    let total_bytes = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total_bytes);
    let mut enc = device.create_encoder("gr-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer");
    let f16s: &[f16] = unsafe { std::slice::from_raw_parts(ptr.cast::<f16>(), (W * H * 4) as usize) };
    f16s[0].to_f32()
}

fn read_rgb_center(device: &GpuDevice, texture: &GpuTexture) -> [f32; 3] {
    let bytes_per_row = W * 8;
    let total_bytes = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total_bytes);
    let mut enc = device.create_encoder("gr-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer");
    let f16s: &[f16] = unsafe { std::slice::from_raw_parts(ptr.cast::<f16>(), (W * H * 4) as usize) };
    [f16s[0].to_f32(), f16s[1].to_f32(), f16s[2].to_f32()]
}

/// Make a shared buffer with identity obj_motion content.
fn make_identity_obj_motion(device: &GpuDevice) -> manifold_gpu::GpuBuffer {
    let buf = device.create_buffer_shared(size_of::<[[f32; 4]; 4]>() as u64);
    let ptr = buf.mapped_ptr().expect("shared buffer");
    let ident: [[f32; 4]; 4] = IDENTITY;
    unsafe { std::ptr::copy_nonoverlapping(&ident as *const _ as *const u8, ptr, size_of::<[[f32; 4]; 4]>()); }
    buf
}

/// Step-response oracle: converge on A, step to B with lighting_changed,
/// measure frames to 90% of step. Runs with gesture flags to exercise the
/// full gesture path.
#[test]
fn step_response_reaches_90_percent_within_4_frames() {
    let h = shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);
    let depth = make_depth_at(device, 0.5);
    let normal = make_pass_through(device, 0.0, 1.0, 0.0, "sr-normal");
    let sv = make_pass_through(device, 1.0, 1.0, 1.0, "sr-sv");
    let sv2 = make_pass_through(device, 1.0, 1.0, 1.0, "sr-sv2");
    let svt = make_pass_through(device, 1.0, 1.0, 1.0, "sr-svt");
    let refl = make_pass_through(device, 0.0, 0.0, 0.0, "sr-refl");
    let gi_materials_buf = device.create_buffer_shared(std::mem::size_of::<GiMaterial>() as u64);
    let obj_motion_buf = make_identity_obj_motion(device);
    let mut history = FullHistorySet::new(device);

    // Warm up: constant irradiance 0.1 for 20 frames (converges to ~0.1).
    let warm_irr = upload_irr(device, 0.1, 0.1, 0.1);
    run_accumulate_frame(
        device, &tracer, &warm_irr, &depth, &normal, &sv, &sv2, &svt, &refl,
        &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, true, 0, "sr-w-0",
    );
    for i in 1..20 {
        run_accumulate_frame(
            device, &tracer, &warm_irr, &depth, &normal, &sv, &sv2, &svt, &refl,
            &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false, 0,
            &format!("sr-w-{i}"),
        );
    }

    let converged_r = read_r_center(device, history.read_irr());
    assert!((converged_r - 0.1).abs() < 0.02, "converged r={converged_r:.4}");

    // Step: change irradiance to 0.5, lighting_changed.
    let step_irr = upload_irr(device, 0.5, 0.5, 0.5);
    // Frame 0: lighting_changed (no gesture yet — first change).
    run_accumulate_frame(
        device, &tracer, &step_irr, &depth, &normal, &sv, &sv2, &svt, &refl,
        &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false,
        STEP_FLAG, "sr-step-0",
    );
    let f0 = read_r_center(device, history.read_irr());
    let step_mag = 0.4;
    let f0_pct = (f0 - converged_r) / step_mag;
    assert!(f0_pct > 0.4, "frame 0: {f0_pct:.3} of step");

    // Frame 1: second consecutive change → gesture arms, n=2 held.
    run_accumulate_frame(
        device, &tracer, &step_irr, &depth, &normal, &sv, &sv2, &svt, &refl,
        &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false,
        STEP_FLAG | GESTURE_FLAG, "sr-step-1",
    );
    let f1 = read_r_center(device, history.read_irr());
    let f1_pct = (f1 - converged_r) / step_mag;
    assert!(f1_pct > 0.65, "frame 1: {f1_pct:.3} of step");

    // Frame 2: gesture hangover (counter=1, still n=2).
    run_accumulate_frame(
        device, &tracer, &step_irr, &depth, &normal, &sv, &sv2, &svt, &refl,
        &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false,
        GESTURE_FLAG, "sr-step-2",
    );
    let f2 = read_r_center(device, history.read_irr());
    let f2_pct = (f2 - converged_r) / step_mag;
    assert!(f2_pct > 0.80, "frame 2: {f2_pct:.3} of step");

    // Frame 3: gesture expired, normal convergence resumes.
    run_accumulate_frame(
        device, &tracer, &step_irr, &depth, &normal, &sv, &sv2, &svt, &refl,
        &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false,
        0, "sr-step-3",
    );
    let f3 = read_r_center(device, history.read_irr());
    let f3_pct = (f3 - converged_r) / step_mag;
    assert!(f3_pct > 0.90, "frame 3: {f3_pct:.3} of step (must reach >90%)");
}

/// Sub-threshold scrub oracle: continuous small per-frame deltas (simulating
/// a slow sun-direction move). Assert per-frame tracking error is bounded.
#[test]
fn sub_threshold_scrub_tracking_error_bounded() {
    let h = shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);
    let depth = make_depth_at(device, 0.5);
    let normal = make_pass_through(device, 0.0, 1.0, 0.0, "so-normal");
    let sv = make_pass_through(device, 1.0, 1.0, 1.0, "so-sv");
    let sv2 = make_pass_through(device, 1.0, 1.0, 1.0, "so-sv2");
    let svt = make_pass_through(device, 1.0, 1.0, 1.0, "so-svt");
    let refl = make_pass_through(device, 0.0, 0.0, 0.0, "so-refl");
    let gi_materials_buf = device.create_buffer_shared(std::mem::size_of::<GiMaterial>() as u64);
    let obj_motion_buf = make_identity_obj_motion(device);
    let mut history = FullHistorySet::new(device);

    // Warm up from 0.05.
    let warm_irr = upload_irr(device, 0.05, 0.05, 0.05);
    run_accumulate_frame(
        device, &tracer, &warm_irr, &depth, &normal, &sv, &sv2, &svt, &refl,
        &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, true, 0, "so-w-0",
    );
    for i in 1..30 {
        run_accumulate_frame(
            device, &tracer, &warm_irr, &depth, &normal, &sv, &sv2, &svt, &refl,
            &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false, 0,
            &format!("so-w-{i}"),
        );
    }

    // Scrub: ramp 0.05→0.55 over 20 frames (0.025/step), with lighting_changed
    // each frame. Gesture arms on frame 1, holds through the ramp.
    let mut prev_was_changed = false;
    for frame in 0..20 {
        let val = 0.05 + (frame as f32 + 1.0) * 0.025;
        let ramp_irr = upload_irr(device, val, val, val);
        let mut flags: u32 = STEP_FLAG;
        if prev_was_changed {
            flags |= GESTURE_FLAG;
        }
        run_accumulate_frame(
            device, &tracer, &ramp_irr, &depth, &normal, &sv, &sv2, &svt, &refl,
            &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false,
            flags, &format!("so-ramp-{frame}"),
        );
        prev_was_changed = true;
        let accumulated = read_r_center(device, history.read_irr());
        let error = (accumulated - val).abs();
        // After gesture active (frame 2+, n=2), tracking error bounded.
        // Bound: 0.08 — the n=2 steady-state ceiling for 0.025/frame ramp.
        if frame >= 2 {
            assert!(error < 0.08, "frame {frame}: err {error:.4}, acc {accumulated:.4}, input {val:.4}");
        }
    }

    let final_val = read_r_center(device, history.read_irr());
    let target = 0.05 + 20.0 * 0.025; // 0.55
    assert!((final_val - target).abs() < 0.06, "final {final_val:.4} vs target {target:.4}");
}

/// SV-channel leg (i): continuous sun-direction move — sv follows within
/// 3 frames (2-frame snap + 1 frame latency). Exercises `cpu_geo_gesture`.
#[test]
fn sv_channel_follows_geometry_move_within_3_frames() {
    let h = shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);
    let depth = make_depth_at(device, 0.5);
    let normal = make_pass_through(device, 0.0, 1.0, 0.0, "sgm-normal");
    let refl = make_pass_through(device, 0.0, 0.0, 0.0, "sgm-refl");
    let sv2 = make_pass_through(device, 1.0, 1.0, 1.0, "sgm-sv2");
    let svt = make_pass_through(device, 1.0, 1.0, 1.0, "sgm-svt");
    let gi_materials_buf = device.create_buffer_shared(std::mem::size_of::<GiMaterial>() as u64);
    let obj_motion_buf = make_identity_obj_motion(device);
    let mut history = FullHistorySet::new(device);

    // Converge on sv=1.0 (fully lit).
    let sv_one = make_pass_through(device, 1.0, 1.0, 1.0, "sgm-sv-one");
    let irr_one = upload_irr(device, 0.5, 0.5, 0.5);
    run_accumulate_frame(
        device, &tracer, &irr_one, &depth, &normal, &sv_one, &sv2, &svt, &refl,
        &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, true, 0, "sgm-w-0",
    );
    for i in 1..20 {
        run_accumulate_frame(
            device, &tracer, &irr_one, &depth, &normal, &sv_one, &sv2, &svt, &refl,
            &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false, 0,
            &format!("sgm-w-{i}"),
        );
    }

    // Move to sv=0.0 (fully shadowed) with geo_gesture.
    let sv_zero = make_pass_through(device, 0.0, 0.0, 0.0, "sgm-sv-zero");
    // Frame 0: geo_changed (single change — no gesture yet, sigma gate path).
    run_accumulate_frame(
        device, &tracer, &irr_one, &depth, &normal, &sv_zero, &sv2, &svt, &refl,
        &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false,
        GEO_CHANGED_FLAG, "sgm-step-0",
    );
    // Frame 1: geo_gesture arms — sv channel snaps via gesture cue.
    run_accumulate_frame(
        device, &tracer, &irr_one, &depth, &normal, &sv_zero, &sv2, &svt, &refl,
        &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false,
        GEO_CHANGED_FLAG | GEO_GESTURE_FLAG, "sgm-step-1",
    );
    // Frame 2: gesture hangover (counter=1, still snapping).
    run_accumulate_frame(
        device, &tracer, &irr_one, &depth, &normal, &sv_zero, &sv2, &svt, &refl,
        &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false,
        GEO_GESTURE_FLAG, "sgm-step-2",
    );

    // sv should be: frame1 0.5 vs 0.0, frame2 0.25. Converging toward 0.
    let sv_out = read_r_center(device, history.read_sv());
    assert!(sv_out < 0.30, "sv after 3-frame geo move: {sv_out:.4}, expected < 0.30");
}

/// SV-channel leg (ii): pure intensity/color strobe — sv convergence is NOT
/// discarded. Full-key flag (STEP_FLAG) should not trip the sv gate.
#[test]
fn sv_channel_holds_through_strobe() {
    let h = shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);
    let depth = make_depth_at(device, 0.5);
    let normal = make_pass_through(device, 0.0, 1.0, 0.0, "ss-normal");
    let refl = make_pass_through(device, 0.0, 0.0, 0.0, "ss-refl");
    let sv2 = make_pass_through(device, 1.0, 1.0, 1.0, "ss-sv2");
    let svt = make_pass_through(device, 1.0, 1.0, 1.0, "ss-svt");
    let gi_materials_buf = device.create_buffer_shared(std::mem::size_of::<GiMaterial>() as u64);
    let obj_motion_buf = make_identity_obj_motion(device);
    let mut history = FullHistorySet::new(device);

    // Converge on sv=0.5 (penumbra), irr=0.3.
    let sv_half = make_pass_through(device, 0.5, 0.5, 0.5, "ss-sv-half");
    let irr_base = upload_irr(device, 0.3, 0.3, 0.3);
    run_accumulate_frame(
        device, &tracer, &irr_base, &depth, &normal, &sv_half, &sv2, &svt, &refl,
        &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, true, 0, "ss-w-0",
    );
    for i in 1..30 {
        run_accumulate_frame(
            device, &tracer, &irr_base, &depth, &normal, &sv_half, &sv2, &svt, &refl,
            &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false, 0,
            &format!("ss-w-{i}"),
        );
    }
    let sv_before = read_r_center(device, history.read_sv());

    // Strobe: change irr intensity (0.3→0.8) with full-key change + gesture.
    // sv stays at 0.5 (visibility unchanged by color/intensity).
    let irr_strobe = upload_irr(device, 0.8, 0.8, 0.8);
    for frame in 0..3 {
        let mut flags = STEP_FLAG;
        if frame > 0 {
            flags |= GESTURE_FLAG;
        }
        run_accumulate_frame(
            device, &tracer, &irr_strobe, &depth, &normal, &sv_half, &sv2, &svt, &refl,
            &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false,
            flags, &format!("ss-strobe-{frame}"),
        );
    }

    let sv_after = read_r_center(device, history.read_sv());
    let sv_delta = (sv_after - sv_before).abs();
    assert!(sv_delta < 0.05, "sv collapse on strobe: {sv_before:.4}→{sv_after:.4}");

    // Irradiance SHOULD have moved.
    let irr_after = read_r_center(device, history.read_irr());
    assert!(irr_after > 0.5, "irradiance must track strobe: {irr_after:.4}");
}

/// SVT leg (iii): tint holds through a strobe. A pure color/intensity strobe
/// only flips the full key (not the geo key) — svt must hold convergence.
#[test]
fn svt_tint_holds_through_strobe() {
    let h = shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);
    let depth = make_depth_at(device, 0.5);
    let normal = make_pass_through(device, 0.0, 1.0, 0.0, "sh-normal");
    let sv = make_pass_through(device, 1.0, 1.0, 1.0, "sh-sv");
    let sv2 = make_pass_through(device, 1.0, 1.0, 1.0, "sh-sv2");
    let refl = make_pass_through(device, 0.0, 0.0, 0.0, "sh-refl");
    let gi_materials_buf = device.create_buffer_shared(std::mem::size_of::<GiMaterial>() as u64);
    let obj_motion_buf = make_identity_obj_motion(device);
    let mut history = FullHistorySet::new(device);

    // Converge on svt=(0.6, 0.3, 0.3) (reddish tint), irr=0.2.
    let svt_red = make_pass_through(device, 0.6, 0.3, 0.3, "sh-svt-red");
    let irr_low = upload_irr(device, 0.2, 0.2, 0.2);
    run_accumulate_frame(
        device, &tracer, &irr_low, &depth, &normal, &sv, &sv2, &svt_red, &refl,
        &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, true, 0, "sh-w-0",
    );
    for i in 1..30 {
        run_accumulate_frame(
            device, &tracer, &irr_low, &depth, &normal, &sv, &sv2, &svt_red, &refl,
            &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false, 0,
            &format!("sh-w-{i}"),
        );
    }
    let svt_before = read_rgb_center(device, history.read_svt());

    // Strobe: change irr (0.2→0.8) with full-key flags only. svt unchanged.
    let irr_high = upload_irr(device, 0.8, 0.8, 0.8);
    for frame in 0..3 {
        let mut flags = STEP_FLAG;
        if frame > 0 {
            flags |= GESTURE_FLAG;
        }
        run_accumulate_frame(
            device, &tracer, &irr_high, &depth, &normal, &sv, &sv2, &svt_red, &refl,
            &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false,
            flags, &format!("sh-strobe-{frame}"),
        );
    }

    let svt_after = read_rgb_center(device, history.read_svt());
    for ch in 0..3 {
        assert!(
            (svt_after[ch] - svt_before[ch]).abs() < 0.05,
            "svt[{ch}] collapsed on strobe: {:.4}→{:.4}",
            svt_before[ch], svt_after[ch]
        );
    }
}

/// Read the moments texture's `.w` (accumulated history length) at the
/// center texel. Rgba32Float = 16 bytes/px.
fn read_moments_w_center(device: &GpuDevice, texture: &GpuTexture) -> f32 {
    let bytes_per_row = W * 16;
    let total_bytes = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total_bytes);
    let mut enc = device.create_encoder("gr-readback-moments");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer");
    let f32s: &[f32] = unsafe { std::slice::from_raw_parts(ptr.cast::<f32>(), (W * H * 4) as usize) };
    f32s[3]
}

/// RAYTRACING_DESIGN.md section 17.7 DN-L gate: with the denoiser's
/// near-raw flag set, the accumulator's history cap drops to n ≤ 4
/// (alpha floor 0.25) — the network's temporal history replaces ours.
/// Control: without the flag the same warmup converges to the 1/alpha
/// cap (20 at TEST_ALPHA 0.05).
#[test]
fn denoise_near_raw_caps_history_at_four_frames() {
    let h = shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);
    let depth = make_depth_at(device, 0.5);
    let normal = make_pass_through(device, 0.0, 1.0, 0.0, "dnl-normal");
    let sv = make_pass_through(device, 1.0, 1.0, 1.0, "dnl-sv");
    let sv2 = make_pass_through(device, 1.0, 1.0, 1.0, "dnl-sv2");
    let svt = make_pass_through(device, 1.0, 1.0, 1.0, "dnl-svt");
    let refl = make_pass_through(device, 0.0, 0.0, 0.0, "dnl-refl");
    let gi_materials_buf = device.create_buffer_shared(std::mem::size_of::<GiMaterial>() as u64);
    let obj_motion_buf = make_identity_obj_motion(device);
    let irr = upload_irr(device, 0.3, 0.3, 0.3);
    const NEAR_RAW: u32 = manifold_gpu::raytrace::ACCUM_FLAG_DENOISE_NEAR_RAW;

    // Control: 30 frames, no flag — history length approaches 1/alpha = 20.
    let mut history = FullHistorySet::new(device);
    run_accumulate_frame(
        device, &tracer, &irr, &depth, &normal, &sv, &sv2, &svt, &refl,
        &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, true, 0, "dnl-c-0",
    );
    for i in 1..30 {
        run_accumulate_frame(
            device, &tracer, &irr, &depth, &normal, &sv, &sv2, &svt, &refl,
            &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false, 0,
            &format!("dnl-c-{i}"),
        );
    }
    let control_len = read_moments_w_center(device, history.read_moments());
    assert!(
        control_len > 12.0,
        "control: 30 unflagged frames must build long history (got {control_len:.2}, cap 20)"
    );

    // Near-raw: same warmup, then 10 flagged frames — the cap must
    // collapse to ≤ 4 (0.25 floor) and hold there.
    let mut history = FullHistorySet::new(device);
    run_accumulate_frame(
        device, &tracer, &irr, &depth, &normal, &sv, &sv2, &svt, &refl,
        &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, true, 0, "dnl-n-0",
    );
    for i in 1..30 {
        run_accumulate_frame(
            device, &tracer, &irr, &depth, &normal, &sv, &sv2, &svt, &refl,
            &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false, 0,
            &format!("dnl-n-{i}"),
        );
    }
    for i in 0..10 {
        run_accumulate_frame(
            device, &tracer, &irr, &depth, &normal, &sv, &sv2, &svt, &refl,
            &mut history, &gi_materials_buf, &obj_motion_buf, TEST_ALPHA, false, NEAR_RAW,
            &format!("dnl-nr-{i}"),
        );
    }
    let near_raw_len = read_moments_w_center(device, history.read_moments());
    assert!(
        near_raw_len <= 4.5,
        "near-raw: flagged frames must cap history at n ≤ 4 (got {near_raw_len:.2})"
    );

    // Value check: constant input means the blend weight change must not
    // move the irradiance value — the cap alters responsiveness, not the
    // converged answer.
    let r = read_r_center(device, history.read_irr());
    assert!((r - 0.3).abs() < 0.02, "converged value drifted: r={r:.4}");
}
