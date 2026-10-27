//! Single-dispatch scalar lattice operation: rebuilds a true distance field from the zero surface,
//! over the nodes of a world-tile pool.
use crate::field_build::FIELD_REDISTANCE_CELLS;
use crate::float_param;
use crate::tile_pool::tile_pool;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::freeze::classify::FusedOutputCapacity;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::READ_SIDE;

use manifold_gpu::GpuBinding;
use std::borrow::Cow;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    cell_size: f32,
    band_cells: f32,
    pool_tiles: f32,
    dispatch_count: u32,
}

manifold_node_engine::primitive! {
    name: RedistanceLattice,
    type_id: "node.redistance_lattice",
    purpose: "Rebuild a node-centred signed distance field from the input zero surface, using the nearest marching-cubes triangle within Band Cells of the cell size. The sign comes from the input; distances outside the band saturate exactly. Node arrays on the world tiles of a pool: one element per pooled node, element ts_cell_at(p) holding node p at (p - 1/2)·cell_size, a node in a tile the table does not hold reading the canonical element. The canonical element and free slots hold the canonical level set, 3 cells.",
    inputs: {
        levelset: Array(f32) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        band_cells: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: { out: Array(f32), },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("band_cells", "Distance Band (Cells)", FIELD_REDISTANCE_CELLS, 0.5, 16.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "levelset is a pooled node array (pool_len(pool_tiles) elements) on the tiles wired into tiles, with pool_tiles and cell_size from the same world tiles (node.surface_tiles for the display). Band Cells sets the band as that many cell sizes, so the depth in cells is the same at any resolution. Never substitute a blur for redistancing.",
    examples: [],
    picker: { label: "Redistance Lattice", category: Atom },
    summary: "Rebuilds a node-centred signed distance field from the input zero surface, so the field holds true distances again.",
    category: Particles3D,
    role: Filter,
    aliases: ["distance field"],
    fusion_kind: Pointwise,
    wgsl_body: concat!(include_str!("shaders/marching_cubes_common.wgsl"), "\n", include_str!("shaders/redistance_lattice_body.wgsl")),
    input_access: [BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "levelset" },
    wgsl_includes: [READ_SIDE],
}

impl Primitive for RedistanceLattice {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "levelset")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Redistance Lattice") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let band_cells = ctx.scalar_or_param("band_cells", FIELD_REDISTANCE_CELLS);
        if !(band_cells.is_finite() && band_cells > 0.0) {
            ctx.error("Redistance Lattice: expected a positive finite band in cells");
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(input), Some(tiles), Some(output)) = (
            ctx.inputs.array("levelset"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let count = pool.elements();
        if count * 4 > input.size.min(output.size) {
            ctx.error(format!(
                "Redistance Lattice: a pool of {} tiles is larger than its arrays",
                pool.pool_tiles
            ));
            return;
        }
        let uniforms = Uniforms {
            cell_size: pool.cell_size,
            band_cells,
            pool_tiles: pool.pool_tiles as f32,
            dispatch_count: count as u32,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: input,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: output,
                    offset: 0,
                },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.redistance_lattice",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;

/// The marching-cubes tables and helpers node.redistance_lattice includes.
pub const MARCHING_CUBES_COMMON: &str = include_str!("shaders/marching_cubes_common.wgsl");
