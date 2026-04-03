// Schwarzschild black hole generator — analytic deflection LUT.
//
// Architecture:
//   Startup: precompute 1D deflection LUT on CPU (512 entries).
//            Maps impact parameter b → total deflection angle.
//   Every frame: single compute dispatch. Per pixel:
//     1. Camera matrix → ray direction → impact parameter
//     2. Sample LUT → deflection angle
//     3. Analytic disk intersection from deflection + camera geometry
//     4. Shade with noise/Doppler/rings
//
// All camera/disk param changes are instant — no rebaking.

use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

const SPEED: usize = 0;
const CAM_DIST: usize = 1;
const TILT: usize = 2;
const ROTATE: usize = 3;
#[allow(dead_code)]
const STEPS: usize = 4; // Kept for param index alignment
const DISK_INNER: usize = 5;
const DISK_OUTER: usize = 6;
const DISK_GLOW: usize = 7;
const SCALE: usize = 8;

const LUT_SIZE: u32 = 512;
// Impact parameter range: from just outside photon sphere (b=2.6rs) to far field
const B_MIN: f32 = 2.598; // Critical impact parameter (photon sphere capture)
const B_MAX: f32 = 30.0;

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 {
        ctx.params[idx]
    } else {
        default
    }
}

/// Numerically compute the total deflection angle for a photon with
/// impact parameter `b` in Schwarzschild spacetime (rs = 1).
/// Uses the effective potential approach with Verlet integration.
fn compute_deflection(b: f32) -> f32 {
    // Integrate in polar coords: u = 1/r, du/dphi = ...
    // The equation of motion for u(phi) is:
    //   d²u/dφ² = -u + 1.5 * u²  (Schwarzschild, rs = 1)
    // with u(0) = 0, du/dφ(0) = 1/b (ray from infinity)

    let mut u = 0.0001_f32; // u = 1/r, start near infinity
    let mut du = 1.0 / b; // du/dphi at infinity
    let mut phi = 0.0_f32;
    let dphi = 0.001_f32; // integration step in phi

    // Integrate until u starts decreasing (ray has passed closest approach)
    // and then continues to infinity, or u diverges (captured)
    let mut max_u = 0.0_f32;
    let mut passed_closest = false;

    for _ in 0..50_000 {
        // Verlet integration of d²u/dφ² = -u + 1.5 * u²
        let accel = -u + 1.5 * u * u;
        du += accel * dphi;
        u += du * dphi;
        phi += dphi;

        if u > max_u {
            max_u = u;
        } else if !passed_closest {
            passed_closest = true;
        }

        // Ray captured (hit horizon)
        if u > 1.0 {
            return std::f32::consts::PI * 10.0; // Sentinel: captured
        }

        // Ray escaped back to infinity
        if passed_closest && u < 0.0001 {
            break;
        }

        // Safety: don't integrate forever
        if phi > 4.0 * std::f32::consts::PI {
            break;
        }
    }

    // Total deflection = phi - pi (pi is the straight-line baseline)
    phi - std::f32::consts::PI
}

/// Build the 1D deflection LUT as f32 array.
fn build_deflection_lut() -> Vec<f32> {
    let mut lut = Vec::with_capacity(LUT_SIZE as usize);
    for i in 0..LUT_SIZE {
        let t = i as f32 / (LUT_SIZE - 1) as f32;
        // Non-linear mapping: more samples near b_min where deflection changes rapidly
        let b = B_MIN + (B_MAX - B_MIN) * t * t; // Quadratic spacing
        let deflection = compute_deflection(b);
        lut.push(deflection);
    }
    lut
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    time_val: f32,
    aspect: f32,
    cam_dist: f32,
    tilt_rad: f32,
    rotate_rad: f32,
    disk_inner: f32,
    disk_outer: f32,
    disk_glow: f32,
    uv_scale: f32,
    orbit_angle: f32,
    b_min: f32,
    b_max: f32,
}

pub struct BlackHoleGenerator {
    pipeline: manifold_gpu::GpuComputePipeline,
    lut_buffer: manifold_gpu::GpuBuffer,
}

impl BlackHoleGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let pipeline = device.create_compute_pipeline(
            include_str!("shaders/black_hole_compute.wgsl"),
            "cs_main",
            "BlackHole",
        );

        // Precompute deflection LUT on CPU and upload to shared buffer
        let lut_data = build_deflection_lut();
        let lut_buffer = device.create_buffer_shared((LUT_SIZE as u64) * 4);
        unsafe {
            let ptr = lut_buffer.mapped_ptr().unwrap();
            std::ptr::copy_nonoverlapping(
                lut_data.as_ptr() as *const u8,
                ptr,
                (LUT_SIZE as usize) * 4,
            );
        }

        Self {
            pipeline,
            lut_buffer,
        }
    }
}

impl Generator for BlackHoleGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::BLACK_HOLE
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        if ctx.param_count == 0 {
            return ctx.anim_progress;
        }

        let speed = param(ctx, SPEED, 0.3);
        let cam_dist = param(ctx, CAM_DIST, 20.0);
        let tilt_deg = param(ctx, TILT, 15.0);
        let rotate_deg = param(ctx, ROTATE, 0.0);
        let disk_inner = param(ctx, DISK_INNER, 3.0);
        let disk_outer = param(ctx, DISK_OUTER, 10.0);
        let disk_glow = param(ctx, DISK_GLOW, 2.0);
        let scale = param(ctx, SCALE, 1.0);

        let uniforms = Uniforms {
            time_val: ctx.time as f32,
            aspect: ctx.aspect,
            cam_dist,
            tilt_rad: tilt_deg.to_radians(),
            rotate_rad: rotate_deg.to_radians(),
            disk_inner,
            disk_outer,
            disk_glow,
            uv_scale: if scale > 0.0 { 1.0 / scale } else { 1.0 },
            orbit_angle: ctx.time as f32 * speed * 0.3,
            b_min: B_MIN,
            b_max: B_MAX,
        };

        gpu.native_enc.dispatch_compute(
            &self.pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 1,
                    buffer: &self.lut_buffer,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: target,
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "BlackHole",
        );

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {}

    fn internal_resolution_scale(&self) -> f32 {
        0.75
    }
}
