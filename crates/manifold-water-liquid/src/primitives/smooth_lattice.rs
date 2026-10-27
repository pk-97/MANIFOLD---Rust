//! `node.smooth_lattice` — binomial smoothing of a scalar lattice held in an
//! `Array(f32)`: the liquid level set inside each step of the field
//! smoothing, between `node.particle_volume` and `node.field_smooth_blend`. One axis per
//! atom; chained over x, y and z it is the full 3D binomial blur at 3·(2p + 1)
//! taps per node instead of (2p + 1)³. A per-element gather on the codegen path,
//! over the nodes of a world-tile pool.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::float_param;
use crate::tile_pool::tile_pool;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::freeze::classify::FusedOutputCapacity;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::READ_SIDE;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SmoothUniforms {
    cell_size: f32,
    passes: f32,
    axis: i32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

manifold_node_engine::primitive! {
    name: SmoothLattice,
    type_id: "node.smooth_lattice",
    purpose: "Smooth a scalar lattice held in an Array<f32> along one axis: `passes` rounds of the [1, 2, 1] / 4 filter, applied as one (2·passes + 1)-tap binomial gather. Chained over axes 0, 1 and 2 it is the full 3D binomial blur. Node arrays on the world tiles of a pool: one element per pooled node, element ts_cell_at(p) holding node p; a neighbour in a tile the table does not hold reads the canonical element. The canonical element and free slots hold the canonical level set, 3 cells. 0 passes copies the input.",
    inputs: {
        levelset: Array(f32) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        passes: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        smoothed: Array(f32),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("passes", "Smoothing Passes", 2.0, 0.0, 3.0),
        ParamDef {
            name: Cow::Borrowed("axis"),
            label: "Axis",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Chain three, axes 0, 1 and 2, inside each step of the field smoothing, between node.particle_volume and node.field_smooth_blend, with tiles, pool_tiles and cell_size from the same world tiles as the level set (node.surface_tiles for the display) and one passes value wired into all three. More passes round the liquid surface and thin sheets further. Live: changing passes reshapes the next frame without touching the simulation.",
    examples: [],
    picker: { label: "Smooth Lattice", category: Atom },
    summary: "Softens a liquid's density field so its surface comes out smooth instead of lumpy.",
    category: Particles3D,
    role: Filter,
    aliases: ["blur volume", "smooth level set", "binomial blur", "lattice blur"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/smooth_lattice_body.wgsl"),
    input_access: [BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "levelset" },
    wgsl_includes: [READ_SIDE],
}

impl Primitive for SmoothLattice {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "smoothed")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "levelset")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Smooth Lattice") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let passes = ctx.scalar_or_param("passes", 2.0).round().clamp(0.0, 3.0);
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Float(n)) => n.round().clamp(0.0, 2.0) as i32,
            _ => 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(levelset), Some(tiles), Some(smoothed)) = (
            ctx.inputs.array("levelset"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("smoothed"),
        ) else {
            return;
        };
        let count = pool.elements();
        if count * 4 > levelset.size.min(smoothed.size) {
            ctx.error(format!(
                "Smooth Lattice: a pool of {} tiles is larger than its arrays",
                pool.pool_tiles
            ));
            return;
        }
        let uniforms = SmoothUniforms {
            cell_size: pool.cell_size,
            passes,
            axis,
            pool_tiles: pool.pool_tiles as f32,
            dispatch_count: count as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
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
                    buffer: levelset,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: smoothed,
                    offset: 0,
                },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.smooth_lattice",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
