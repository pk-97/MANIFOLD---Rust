//! One upwind reinitialisation sweep, ported from FLIP Fluids levelsetsolver.cpp.
//! Copyright Ryan L. Guy & Dennis Fassbaender (MIT); THIRD_PARTY_NOTICES.md.
//! The engine's upwind rule, not a nearest-triangle distance. This atom is a
//! pure stencil sweep over pooled cell arrays on the step's world tiles;
//! whitewater owns convergence reduction and calls its generated pipeline.
use crate::float_param;
use crate::tile_pool::tile_pool;
use manifold_gpu::GpuBinding;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use std::borrow::Cow;

/// The generated sweep's uniforms, uploaded by this node and by the
/// whitewater stage's convergence loop.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct UpwindUniforms {
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
}

impl UpwindUniforms {
    pub(super) fn new(cell_size: f32, pool_tiles: u32, dispatch_count: u32) -> Self {
        Self {
            cell_size,
            pool_tiles: pool_tiles as f32,
            dispatch_count,
            _pad0: 0,
        }
    }
}

manifold_node_engine::primitive! {
    name: UpwindDistance,
    type_id: "node.upwind_distance",
    purpose: "One FLIP Fluids upwind signed-distance sweep on valid cells, with recomputed smoothed sign, clamped neighbours and pseudo-time h/2. Invalid cells retain their input. Cell arrays on the world tiles of a pool: one element per pooled cell, a neighbour in a tile the table does not hold reading the canonical element.",
    inputs: {
        levelset: Array(f32) required,
        valid: Array(u32) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: { out: Array(f32), },
    params: [
        float_param!("cell_size", "Cell Size", 1.0, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Compose repeated sweeps with an engine convergence reduction; whitewater uses six maximum sweeps including the final converging sweep. levelset and valid are pooled cell arrays (pool_len(pool_tiles) elements) on the liquid step's tiles, wired with pool_tiles. This operation does not perform geometric nearest-triangle redistancing.",
    examples: [],
    summary: "One FLIP Fluids upwind signed-distance sweep on valid cells, with recomputed smoothed sign, clamped neighbours and pseudo-time h/2. Invalid cells retain their input.",
    category: Particles3D,
    role: Filter,
    aliases: ["distance field", "upwind reinitialisation"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/upwind_distance_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather],
    wgsl_includes: [READ_SIDE],
}

impl Primitive for UpwindDistance {
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
        let pool = match tile_pool(ctx, "Upwind Distance") {
            Ok(pool) => pool,
            Err(refusal) => {
                ctx.error(refusal);
                return;
            }
        };
        let (Some(input), Some(output), Some(valid), Some(tiles)) = (
            ctx.inputs.array("levelset"),
            ctx.outputs.array("out"),
            ctx.inputs.array("valid"),
            ctx.inputs.array("tiles"),
        ) else {
            return;
        };
        let elements = pool.elements();
        if elements * 4 > input.size.min(output.size).min(valid.size) {
            ctx.error("Upwind Distance: the arrays hold fewer elements than the pool");
            return;
        }
        let uniforms = UpwindUniforms::new(pool.cell_size, pool.pool_tiles, elements as u32);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
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
                    buffer: valid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: output,
                    offset: 0,
                },
            ],
            [(elements as u32).div_ceil(256), 1, 1],
            "node.upwind_distance",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
