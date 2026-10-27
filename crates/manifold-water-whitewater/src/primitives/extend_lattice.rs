//! `node.extend_lattice` — one layer of FLIP's grid extrapolation into the
//! unknown cells of a whitewater field on the liquid step's world tiles. A
//! per-element gather on the codegen path.
//!
//! Ported from FLIP Fluids gridutils.h (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::float_param;
use manifold_water_liquid::tile_pool::tile_pool;
use manifold_water_liquid::whitewater::{KnownValue, WHITEWATER_COMMON};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ExtendUniforms {
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
}

manifold_node_engine::primitive! {
    name: ExtendLattice,
    type_id: "node.extend_lattice",
    purpose: "One layer of FLIP's grid extrapolation on a whitewater field over the liquid step's world tiles: a known cell keeps its value; an unknown cell with a known face neighbour becomes known with the mean of its known face neighbours. Others stay as they are. A neighbour in a tile the table does not hold reads the canonical element, unknown, so the field grows as far as its tiles go.",
    inputs: {
        values: Array(KnownValue) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        out: Array(KnownValue),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Chain one per layer, each reading the one before: three after node.lattice_curvature, as FLIP extends its curvature grid. tiles and pool_tiles are the liquid step's, cell_size the domain's.",
    examples: [],
    summary: "Fills the empty cells next to known ones with their neighbours' average, one cell further each time.",
    category: Particles3D,
    role: Filter,
    aliases: ["extrapolate", "extend field", "fill unknown cells"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/extend_lattice_body.wgsl"),
    input_access: [BufferGather, BufferGather],
    wgsl_includes: [READ_SIDE, WHITEWATER_COMMON],
}

impl Primitive for ExtendLattice {
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
                    .find(|(name, _)| *name == "values")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Extend Lattice") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(values), Some(tiles), Some(out)) = (
            ctx.inputs.array("values"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let count = pool.elements();
        if count * 8 > values.size.min(out.size) {
            ctx.error(format!(
                "Extend Lattice: a pool of {} tiles is larger than its arrays",
                pool.pool_tiles
            ));
            return;
        }
        let uniforms = ExtendUniforms {
            cell_size: pool.cell_size,
            pool_tiles: pool.pool_tiles as f32,
            dispatch_count: count as u32,
            _pad0: 0,
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
                    buffer: values,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: out,
                    offset: 0,
                },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.extend_lattice",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
