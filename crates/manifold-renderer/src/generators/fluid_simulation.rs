// Density-displacement particle compute fluid simulation.
// Thin wrapper around FluidSimCore — extracts parameters from GeneratorContext
// and delegates all GPU work to the shared core.

use super::fluid_sim_core::{FluidSimContext, FluidSimCore, FluidSimParams};
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

use crate::generators::registration::GeneratorFactory;

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::FLUID_SIMULATION,
        create: |device| Box::new(FluidSimulationGenerator::new(device)),
    }
}

// Parameter indices (13 params).
const SLOPE: usize = 0;
const BLUR: usize = 1;
const ROTATION: usize = 2;
const NOISE: usize = 3;
const SPEED: usize = 4;
const CONTRAST: usize = 5;
const SCALE: usize = 6;
const PARTICLES: usize = 7;
const SNAP: usize = 8;
const SNAP_MODE: usize = 9;
const SPLAT_SIZE: usize = 10;
const ANTI_CLUMP: usize = 11;
const INJECT_FORCE: usize = 12;
const FILL: usize = 13;

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 {
        ctx.params[idx]
    } else {
        default
    }
}

pub struct FluidSimulationGenerator {
    core: FluidSimCore,
}

impl FluidSimulationGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            core: FluidSimCore::new(device),
        }
    }
}

impl Generator for FluidSimulationGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::FLUID_SIMULATION
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        let params = FluidSimParams {
            slope: param(ctx, SLOPE, -0.01),
            blur_radius: param(ctx, BLUR, 20.0),
            rotation_deg: param(ctx, ROTATION, 85.0),
            noise: param(ctx, NOISE, 0.001),
            speed: param(ctx, SPEED, 1.0),
            contrast: param(ctx, CONTRAST, 3.0),
            scale: param(ctx, SCALE, 1.0),
            particles_millions: param(ctx, PARTICLES, 2.0),
            snap: param(ctx, SNAP, 0.0),
            snap_mode: param(ctx, SNAP_MODE, 0.0),
            splat_size: param(ctx, SPLAT_SIZE, 3.0),
            anti_clump: param(ctx, ANTI_CLUMP, 20.0),
            inject_force: param(ctx, INJECT_FORCE, 0.005),
            fill: param(ctx, FILL, 1.0),
        };

        let sim_ctx = FluidSimContext {
            width: ctx.width,
            height: ctx.height,
            dt: ctx.dt,
            time: ctx.time,
            trigger_count: ctx.trigger_count,
        };

        self.core.render(gpu, target, &params, &sim_ctx);
        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {
        self.core.resize();
    }

    fn internal_resolution_scale(&self) -> f32 {
        1.0
    }

    fn reset_state(&mut self, _device: &manifold_gpu::GpuDevice) {
        self.core.reset_state();
    }
}
