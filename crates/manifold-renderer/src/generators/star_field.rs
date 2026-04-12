use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

use crate::generators::registration::GeneratorFactory;

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::STAR_FIELD,
        create: |device| Box::new(StarFieldGenerator::new(device)),
    }
}

// Parameter indices
const DENSITY: usize = 0;
const BRIGHTNESS: usize = 1;
const DEPTH: usize = 2;
const DRIFT_SPEED: usize = 3;
const DRIFT_X: usize = 4;
const DRIFT_Y: usize = 5;
const TWINKLE: usize = 6;
const WARMTH: usize = 7;
const GLOW: usize = 8;

const SHADER: &str = include_str!("shaders/star_field.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StarFieldUniforms {
    time_val: f32,
    aspect_ratio: f32,
    density: f32,
    brightness: f32,
    depth: f32,
    drift_speed: f32,
    drift_x: f32,
    drift_y: f32,
    twinkle: f32,
    warmth: f32,
    glow: f32,
    _pad: [f32; 3],
}

pub struct StarFieldGenerator {
    pipeline: manifold_gpu::GpuComputePipeline,
}

impl StarFieldGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            pipeline: device.create_compute_pipeline(SHADER, "cs_main", "Star Field"),
        }
    }
}

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 {
        ctx.params[idx]
    } else {
        default
    }
}

impl Generator for StarFieldGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::STAR_FIELD
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        let uniforms = StarFieldUniforms {
            time_val: ctx.time as f32,
            aspect_ratio: ctx.aspect,
            density: param(ctx, DENSITY, 0.5),
            brightness: param(ctx, BRIGHTNESS, 0.7),
            depth: param(ctx, DEPTH, 0.5),
            drift_speed: param(ctx, DRIFT_SPEED, 0.15),
            drift_x: param(ctx, DRIFT_X, 0.3),
            drift_y: param(ctx, DRIFT_Y, 0.1),
            twinkle: param(ctx, TWINKLE, 0.3),
            warmth: param(ctx, WARMTH, 0.0),
            glow: param(ctx, GLOW, 0.3),
            _pad: [0.0; 3],
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
            "Star Field",
        );

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {}
}
