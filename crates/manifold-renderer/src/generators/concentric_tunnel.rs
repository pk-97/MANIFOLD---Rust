use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

use crate::generators::clip_trigger::ClipTriggerCycle;
use crate::generators::registration::GeneratorFactory;

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::CONCENTRIC_TUNNEL,
        create: |device| Box::new(ConcentricTunnelGenerator::new(device)),
    }
}

const SHAPE: usize = 0;
const LINE: usize = 1;
const SPEED: usize = 2;
const SCALE: usize = 3;
const CLIP_TRIGGER: usize = 4;
const CLIP_TRIGGER_MODE: usize = 5;
const SHAPE_COUNT: u32 = 6;

const MODE_SHAPE: i32 = 0;
const MODE_SPAWN: i32 = 1;
const MODE_BOTH: i32 = 2;

const BEAT_VALUES: [f32; 5] = [0.25, 0.5, 1.0, 2.0, 4.0];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ConcentricTunnelUniforms {
    time: f32,
    beat: f32,
    aspect_ratio: f32,
    line_thickness: f32,
    anim_speed: f32,
    uv_scale: f32,
    shape_type: f32,
    clip_trigger_mode: f32,
    trigger_count: f32,
    _pad: [f32; 3],
}

pub struct ConcentricTunnelGenerator {
    pipeline: manifold_gpu::GpuComputePipeline,
    /// Defense-in-depth uniqueness invariant on the clip-trigger
    /// shape cycle (`trigger_count % SHAPE_COUNT`).
    clip_trigger_cycle: ClipTriggerCycle,
}

impl ConcentricTunnelGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let pipeline = device.create_compute_pipeline(
            include_str!("shaders/concentric_tunnel_compute.wgsl"),
            "cs_main",
            "ConcentricTunnel",
        );
        Self {
            pipeline,
            clip_trigger_cycle: ClipTriggerCycle::new(),
        }
    }
}

impl Generator for ConcentricTunnelGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::CONCENTRIC_TUNNEL
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

        let line = if ctx.param_count > LINE as u32 {
            ctx.params[LINE]
        } else {
            0.008
        };
        let speed_idx = if ctx.param_count > SPEED as u32 {
            (ctx.params[SPEED].round() as usize).min(BEAT_VALUES.len() - 1)
        } else {
            2
        };
        let scale = if ctx.param_count > SCALE as u32 {
            ctx.params[SCALE]
        } else {
            1.0
        };
        let triggered = ctx.param_count > CLIP_TRIGGER as u32 && ctx.params[CLIP_TRIGGER] > 0.5;
        let mode = if ctx.param_count > CLIP_TRIGGER_MODE as u32 {
            (ctx.params[CLIP_TRIGGER_MODE].round() as i32).clamp(MODE_SHAPE, MODE_BOTH)
        } else {
            MODE_SHAPE
        };

        let anim_speed = BEAT_VALUES[speed_idx];

        let mut clip_trigger_mode_shader = 0.0_f32;
        let shape = if triggered {
            let cycle_shape = mode == MODE_SHAPE || mode == MODE_BOTH;
            let spawn_rings = mode == MODE_SPAWN || mode == MODE_BOTH;
            if spawn_rings {
                clip_trigger_mode_shader = if mode == MODE_BOTH { 2.0 } else { 1.0 };
            }
            if cycle_shape {
                self.clip_trigger_cycle
                    .step(ctx.trigger_count, SHAPE_COUNT) as f32
            } else {
                if ctx.param_count > SHAPE as u32 {
                    ctx.params[SHAPE].round()
                } else {
                    0.0
                }
            }
        } else {
            if ctx.param_count > SHAPE as u32 {
                ctx.params[SHAPE].round()
            } else {
                0.0
            }
        };

        let uniforms = ConcentricTunnelUniforms {
            time: ctx.time as f32,
            beat: ctx.beat as f32,
            aspect_ratio: ctx.aspect,
            line_thickness: line,
            anim_speed,
            uv_scale: if scale > 0.0 { 1.0 / scale } else { 1.0 },
            shape_type: shape,
            clip_trigger_mode: clip_trigger_mode_shader,
            trigger_count: ctx.trigger_count as f32,
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
            "ConcentricTunnel",
        );

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {}
}
