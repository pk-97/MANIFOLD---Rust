//! `node.field_close` — a morphological closing of the liquid's level set on
//! the display's world tiles: the liquid grown by one simulation cell, then
//! shrunk by one, so holes and gaps through a thin sheet up to two simulation
//! cells across close at every Surface Detail and every other surface stays
//! where it was. The shrink keeps only what the closing adds where it adds
//! much, so the shallow dips between particles keep their shape instead of
//! taking the cube's. Two runs of one kernel
//! (`shaders/field_close.wgsl`), the grow into a scratch field and the shrink
//! from it into the output. The node owns both and makes them on its first
//! run, so while a switch leaves it out it holds no memory.

use manifold_gpu::BufferUse;
use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::field_build::display_stride;
use manifold_water_liquid::float_param;
use manifold_water_liquid::tile_pool::tile_pool;

// The kernel after the world-tile read side.
static BODY: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!("{READ_SIDE}\n{}", include_str!("shaders/field_close.wgsl"))
});

/// Field order identical to the shader's `Params`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    cell_size: f32,
    pool_tiles: u32,
    _pad_h: u32,
    widest: u32,
    stride: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

manifold_node_engine::primitive! {
    name: FieldClose,
    type_id: "node.field_close",
    purpose: "A morphological closing of the liquid's level set on a world-tile pool: each node takes the least value of the nodes within a simulation cell of it (a cube, sim_cell_size over cell_size nodes each way, a whole number from 1 to 3: the liquid grown by a simulation cell), then each takes the greatest of those (shrunk by one). Holes and gaps through the liquid up to two simulation cells across close at every Surface Detail; every other surface comes back where it was. The shrink keeps only what the closing adds where it adds much: a node the closing moves inward by under 0.15 simulation cells keeps the input level set, one moved by over 0.5 takes the closing, easing between, so shallow dips between particles keep their own shape. Node arrays on the world tiles of a pool, one element per pooled node, read at the same pool index. A node in a tile the table does not hold takes no part. The canonical element and free slots hold the far value, 3 cells.",
    inputs: {
        levelset: Array(f32) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        sim_cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: { field: Array(f32), },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("sim_cell_size", "Simulation Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire levelset from node.particle_volume and tiles, pool_tiles and cell_size from the same world tiles (node.surface_tiles for the display), sim_cell_size from the domain; its field feeds the smoothing (node.field_smooth_blend's prior and reference). It owns its field and a scratch field the size of its input, made on its first run. It reads the nodes round each tile, so it is a graph fusion boundary.",
    examples: [],
    picker: { label: "Field Close", category: Atom },
    summary: "Closes small holes and gaps in a liquid surface, so thin sheets of water hold together.",
    category: Particles3D,
    role: Filter,
    aliases: ["close gaps", "closing", "fill holes", "morphological closing"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        close_pipeline: Option<GpuComputePipeline> = None,
        grown: Option<GpuBuffer> = None,
        closed: Option<GpuBuffer> = None,
    },
}

/// `buffer` made at least `bytes` long, within the device's memory budget.
fn ensure(buffer: &mut Option<GpuBuffer>, device: &GpuDevice, bytes: u64) -> Result<(), String> {
    if buffer.as_ref().is_some_and(|held| held.size >= bytes) {
        return Ok(());
    }
    *buffer = None;
    manifold_node_engine::load::expand::admit_world_bytes(device.modifier_memory_snapshot(), bytes)
        .map_err(|error| error.to_string())?;
    *buffer = Some(device.try_create_buffer(bytes, BufferUse::Gpu)?);
    Ok(())
}

impl Primitive for FieldClose {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "field"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "field" => self.closed.as_ref(),
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
        (port == "field").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Field Close") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let (Some(levelset), Some(tiles)) =
            (ctx.inputs.array("levelset"), ctx.inputs.array("tiles"))
        else {
            ctx.error("Field Close: levelset and tiles must be wired");
            return;
        };
        let stride = match display_stride(
            "Field Close",
            ctx.scalar_or_param("sim_cell_size", 0.0625),
            pool.cell_size,
        ) {
            Ok(stride) => stride,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let bytes = pool.elements() * 4;
        if levelset.size < bytes {
            ctx.error(format!(
                "Field Close: levelset holds {} values but a pool of {} tiles needs {}",
                levelset.size / 4,
                pool.pool_tiles,
                bytes / 4
            ));
            return;
        }
        let gpu = ctx.gpu_encoder();
        let made = ensure(&mut self.grown, gpu.device, bytes)
            .and_then(|()| ensure(&mut self.closed, gpu.device, bytes));
        if let Err(error) = made {
            ctx.error(format!(
                "Field Close: its fields need storage the device cannot give: {error}"
            ));
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = self.close_pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(BODY.as_str(), "cs_main", "node.field_close")
        });
        let (Some(grown), Some(field)) = (self.grown.as_ref(), self.closed.as_ref()) else {
            return;
        };
        for (widest, src, out, label) in [
            (0, levelset, grown, "node.field_close grow"),
            (1, grown, field, "node.field_close shrink"),
        ] {
            let uniforms = Uniforms {
                cell_size: pool.cell_size,
                pool_tiles: pool.pool_tiles,
                _pad_h: 0,
                widest,
                stride,
                _pad0: 0,
                _pad1: 0,
                _pad2: 0,
            };
            let buffer = |binding, buffer| GpuBinding::Buffer {
                binding,
                buffer,
                offset: 0,
            };
            gpu.native_enc.dispatch_compute(
                pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    buffer(1, src),
                    buffer(2, tiles),
                    buffer(3, out),
                    buffer(4, levelset),
                ],
                [pool.pool_tiles + 1, 1, 1],
                label,
            );
            gpu.native_enc.compute_memory_barrier_buffers();
        }
    }
}

#[cfg(test)]
mod cpu_tests {
    use super::*;

    #[test]
    fn composed_shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(&BODY)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&BODY)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(std::mem::size_of::<Uniforms>(), 32);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
