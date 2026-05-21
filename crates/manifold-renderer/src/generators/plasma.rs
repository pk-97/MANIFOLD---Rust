use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

use crate::generators::registration::GeneratorFactory;

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::PLASMA,
        create: |device| Box::new(PlasmaGenerator::new(device)),
    }
}

// Parameter indices matching Unity's PlasmaGenerator.cs
const PATTERN: usize = 0;
const COMPLEXITY: usize = 1;
const CONTRAST: usize = 2;
const SPEED: usize = 3;
const SCALE: usize = 4;
const CLIP_TRIGGER: usize = 5;
const PATTERN_COUNT: u32 = 8;

/// Plasma WGSL source — shared across all specialized pattern variants.
const PLASMA_WGSL: &str = include_str!("shaders/plasma_compute.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PlasmaUniforms {
    time: f32,
    aspect_ratio: f32,
    anim_speed: f32,
    uv_scale: f32,
    pattern_type: f32,
    complexity: f32,
    contrast: f32,
    trigger_count: f32,
}

pub struct PlasmaGenerator {
    /// Specialized pipelines per pattern type. Metal compiler eliminates the
    /// switch in each variant via function constants.
    pipelines: [manifold_gpu::GpuComputePipeline; 8],
}

impl PlasmaGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let names = [
            "Classic", "Rings", "Diamond", "Warp", "Cells", "Noise", "Fractal", "Lattice",
        ];
        let pipelines = std::array::from_fn(|i| {
            let val = format!("{}.0", i);
            device.create_specialized_compute_pipeline(
                PLASMA_WGSL,
                "cs_main",
                &[("u.pattern_type", &val)],
                &format!("Plasma {}", names[i]),
            )
        });
        Self { pipelines }
    }
}

impl Generator for PlasmaGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::PLASMA
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
            1.0
        };
        let scale = if ctx.param_count > SCALE as u32 {
            ctx.params[SCALE]
        } else {
            1.0
        };
        let clip_trigger = ctx.param_count > CLIP_TRIGGER as u32 && ctx.params[CLIP_TRIGGER] > 0.5;

        let pattern_type = if clip_trigger {
            (ctx.trigger_count % PATTERN_COUNT) as f32
        } else if ctx.param_count > PATTERN as u32 {
            ctx.params[PATTERN].round()
        } else {
            0.0
        };

        let uniforms = PlasmaUniforms {
            time: ctx.time as f32,
            aspect_ratio: ctx.aspect,
            anim_speed: speed,
            uv_scale: if scale > 0.0 { 1.0 / scale } else { 1.0 },
            pattern_type,
            complexity: if ctx.param_count > COMPLEXITY as u32 {
                ctx.params[COMPLEXITY]
            } else {
                0.5
            },
            contrast: if ctx.param_count > CONTRAST as u32 {
                ctx.params[CONTRAST]
            } else {
                0.5
            },
            trigger_count: ctx.trigger_count as f32,
        };

        // Select specialized pipeline for this pattern type
        let pattern_idx = (pattern_type.round() as u32).min(PATTERN_COUNT - 1) as usize;
        gpu.native_enc.dispatch_compute(
            &self.pipelines[pattern_idx],
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
            "Plasma Compute",
        );

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {}
}
