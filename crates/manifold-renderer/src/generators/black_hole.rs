use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

// Parameter indices
const SPEED: usize = 0;
const CAM_DIST: usize = 1;
const TILT: usize = 2;
const STEPS: usize = 3;
const DISK_INNER: usize = 4;
const DISK_OUTER: usize = 5;
const DISK_GLOW: usize = 6;
const SCALE: usize = 7;

const SHADER: &str = include_str!("shaders/black_hole_compute.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlackHoleUniforms {
    time_val: f32,
    aspect: f32,
    speed: f32,
    cam_dist: f32,
    tilt_rad: f32,
    steps: f32,
    disk_inner: f32,
    disk_outer: f32,
    disk_glow: f32,
    uv_scale: f32,
    _pad0: f32,
    _pad1: f32,
}

pub struct BlackHoleGenerator {
    pipeline: manifold_gpu::GpuComputePipeline,
}

impl BlackHoleGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            pipeline: device.create_compute_pipeline(SHADER, "cs_main", "Black Hole"),
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

        let speed = if ctx.param_count > SPEED as u32 {
            ctx.params[SPEED]
        } else {
            0.3
        };
        let cam_dist = if ctx.param_count > CAM_DIST as u32 {
            ctx.params[CAM_DIST]
        } else {
            20.0
        };
        let tilt_deg = if ctx.param_count > TILT as u32 {
            ctx.params[TILT]
        } else {
            75.0
        };
        let steps = if ctx.param_count > STEPS as u32 {
            ctx.params[STEPS].round()
        } else {
            200.0
        };
        let disk_inner = if ctx.param_count > DISK_INNER as u32 {
            ctx.params[DISK_INNER]
        } else {
            3.0
        };
        let disk_outer = if ctx.param_count > DISK_OUTER as u32 {
            ctx.params[DISK_OUTER]
        } else {
            10.0
        };
        let disk_glow = if ctx.param_count > DISK_GLOW as u32 {
            ctx.params[DISK_GLOW]
        } else {
            2.0
        };
        let scale = if ctx.param_count > SCALE as u32 {
            ctx.params[SCALE]
        } else {
            1.0
        };

        let uniforms = BlackHoleUniforms {
            time_val: ctx.time as f32,
            aspect: ctx.aspect,
            speed,
            cam_dist,
            tilt_rad: tilt_deg.to_radians(),
            steps,
            disk_inner,
            disk_outer,
            disk_glow,
            uv_scale: if scale > 0.0 { 1.0 / scale } else { 1.0 },
            _pad0: 0.0,
            _pad1: 0.0,
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
            "Black Hole Compute",
        );

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {}

    fn internal_resolution_scale(&self) -> f32 {
        0.75
    }
}
