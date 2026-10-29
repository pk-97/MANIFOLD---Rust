//! `node.water_on_sand` — the water at each grain of a Sand: the water's
//! velocity there and how much of the grain is under water, read from the
//! water's display level set and velocity. The Sand's state reads it on the
//! next frame (a state-capture wire), so the push is kept between frames in
//! storage the node owns.

use std::borrow::Cow;
use std::sync::LazyLock;

use manifold_gpu::BufferUse;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::float_param;
use manifold_water_liquid::tile_pool::tile_pool;

static SHADER: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{READ_SIDE}\n{}",
        include_str!("shaders/water_on_sand.wgsl")
    )
});
const THREADS: u32 = 64;

/// A grain's push: the water's velocity and the share under water.
pub(crate) const PUSH_BYTES: u64 = 16;

/// Field order identical to the shader's `Params`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct WaterOnSandUniforms {
    sand: u32,
    pool_tiles: u32,
    cell: f32,
    pad: u32,
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}

manifold_node_engine::primitive! {
    name: WaterOnSand,
    type_id: "node.water_on_sand",
    purpose: "The water at each grain of a Sand: the water's display level set and velocity, trilinear at the grain on the display tiles, give the water's velocity there and the share of the grain under water (1 half a cell inside the surface, 0 half a cell out), one vec4 a grain in the grains' order. A removed grain, or one whose tile the display does not hold, gets none. The push is kept between frames for the Sand's state, which reads it on the next frame.",
    inputs: {
        sand: Array(FluidParticle) required,
        sand_count: ScalarF32 optional,
        levelset: Array(f32) required,
        velocity: Array(f32) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        push: Array(f32),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire sand and sand_count from the Sand's frame (particles_b, count_b), levelset and velocity from the water's display surface, tiles, pool_tiles and cell_size from its node.surface_tiles; push feeds node.matter_state's water_push.",
    examples: [],
    picker: { label: "Water on Sand", category: Atom },
    summary: "Finds the water at each sand grain, so the water can push and drag the sand.",
    category: Particles3D,
    role: Filter,
    aliases: ["water on sand", "sand drag", "sand push"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        kernel: Option<GpuComputePipeline> = None,
        push: Option<GpuBuffer> = None,
    },
}

impl WaterOnSand {
    /// The push for `grains`, at least one record; remade only when it grows.
    fn ensure_push(&mut self, device: &GpuDevice, grains: u64) -> Result<(), String> {
        let bytes = grains.max(1) * PUSH_BYTES;
        if self.push.as_ref().is_some_and(|b| b.size >= bytes) {
            return Ok(());
        }
        self.push = None;
        manifold_node_engine::load::expand::admit_world_bytes(
            device.modifier_memory_snapshot(),
            bytes,
        )
        .map_err(|error| error.to_string())?;
        let push = device.try_create_buffer(bytes, BufferUse::Upload)?;
        push.zero_fill();
        self.push = Some(push);
        Ok(())
    }
}

impl Primitive for WaterOnSand {
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        self.kernel.get_or_insert_with(|| {
            device.create_compute_pipeline(SHADER.as_str(), "cs_push", "node.water_on_sand")
        });
    }

    fn provides_array_output(&self, port: &str) -> bool {
        port == "push"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "push" => self.push.as_ref(),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-record hint, made at the first run.
        (port == "push").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Water on Sand") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let (Some(sand), Some(levelset), Some(velocity), Some(tiles)) = (
            ctx.inputs.array("sand"),
            ctx.inputs.array("levelset"),
            ctx.inputs.array("velocity"),
            ctx.inputs.array("tiles"),
        ) else {
            ctx.error("Water on Sand: sand, levelset, velocity and tiles must be wired");
            return;
        };
        if levelset.size < pool.elements() * 4 || velocity.size < pool.elements() * 12 {
            ctx.error(format!(
                "Water on Sand: the level set and velocity need {} values each (velocity three) for a pool of {} tiles",
                pool.elements(),
                pool.pool_tiles
            ));
            return;
        }
        let slots = sand.size / size_of::<FluidParticle>() as u64;
        let count = (ctx.scalar_or_param("sand_count", 0.0).round().max(0.0) as u64).min(slots);
        let prepared = {
            let device = ctx.gpu_encoder().device;
            self.kernel.get_or_insert_with(|| {
                device.create_compute_pipeline(SHADER.as_str(), "cs_push", "node.water_on_sand")
            });
            self.ensure_push(device, slots)
        };
        if let Err(error) = prepared {
            ctx.error(format!(
                "Water on Sand: the push needs storage the device cannot give: {error}"
            ));
            return;
        }
        let (Some(kernel), Some(push)) = (self.kernel.as_ref(), self.push.as_ref()) else {
            return;
        };
        if count == 0 {
            return;
        }
        let uniforms = WaterOnSandUniforms {
            sand: count as u32,
            pool_tiles: pool.pool_tiles,
            cell: pool.cell_size,
            pad: 0,
        };
        ctx.gpu_encoder().native_enc.dispatch_compute(
            kernel,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                buffer(1, sand),
                buffer(2, levelset),
                buffer(3, velocity),
                buffer(4, tiles),
                buffer(5, push),
            ],
            [(count as u32).div_ceil(THREADS), 1, 1],
            "node.water_on_sand",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(&SHADER).expect("water on sand shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("water on sand shader validates");
        assert!(module.entry_points.iter().any(|e| e.name == "cs_push"));
        assert_eq!(std::mem::size_of::<WaterOnSandUniforms>(), 16);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
