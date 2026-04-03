// Schwarzschild black hole generator — single-pass, every-frame integration.
//
// Runs a short geodesic integration (50 steps) per pixel every frame.
// No deflection map, no rebaking. All params are instant.
// 50 steps at 0.75x resolution ≈ 25M iterations/frame — fast on Apple Silicon.

use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

const SPEED: usize = 0;
const CAM_DIST: usize = 1;
const TILT: usize = 2;
const ROTATE: usize = 3;
#[allow(dead_code)]
const STEPS: usize = 4;
const DISK_INNER: usize = 5;
const DISK_OUTER: usize = 6;
const DISK_GLOW: usize = 7;
const SCALE: usize = 8;

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 {
        ctx.params[idx]
    } else {
        default
    }
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
    steps: f32,
    _pad0: f32,
}

pub struct BlackHoleGenerator {
    pipeline: manifold_gpu::GpuComputePipeline,
}

impl BlackHoleGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            pipeline: device.create_compute_pipeline(
                include_str!("shaders/black_hole_compute.wgsl"),
                "cs_main",
                "BlackHole",
            ),
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
        let steps = param(ctx, STEPS, 200.0).round();
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
            steps,
            _pad0: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            &self.pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
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
