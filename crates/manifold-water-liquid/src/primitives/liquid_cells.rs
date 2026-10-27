//! `node.liquid_cells` — FLIP's emitter material grid on the liquid step's
//! world tiles: air, liquid or solid per cell, the liquid shrunk off the air.
//! A per-element gather on the codegen path.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::float_param;
use crate::tile_pool::tile_pool;
use crate::whitewater::WHITEWATER_COMMON;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::READ_SIDE;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CellsUniforms {
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
}

manifold_node_engine::primitive! {
    name: LiquidCells,
    type_id: "node.liquid_cells",
    purpose: "Mark each world cell air (0), liquid (1) or solid (2) as FLIP's whitewater emitter does: solid where the mean of the cell's eight solid corners is below 0, else liquid where its distance is below 0, else air; then a liquid cell with an air face neighbour becomes air, so emitters sit in the surface layer's air. Cell distance and the solid nodes (node n at the low corner of cell n) are read through the liquid step's world tiles; a cell or node in a tile the table does not hold reads the canonical element: air, not solid.",
    inputs: {
        distance: Array(f32) required,
        solid: Array(f32) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        out: Array(u32),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "distance the liquid distance and solid the solid nodes, one float per pooled world cell, with tiles and pool_tiles from the same liquid step and cell_size the domain's. The whitewater's wavecrest and type rules read it.",
    examples: [],
    summary: "Sorts every grid cell into air, liquid or wall, the map whitewater uses to decide where spray and foam can form.",
    category: Particles3D,
    role: Filter,
    aliases: ["material grid", "cell types", "fluid mask"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/liquid_cells_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather],
    wgsl_includes: [READ_SIDE, WHITEWATER_COMMON],
}

impl Primitive for LiquidCells {
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
                    .find(|(name, _)| *name == "distance")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Liquid Cells") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(distance), Some(solid), Some(tiles), Some(out)) = (
            ctx.inputs.array("distance"),
            ctx.inputs.array("solid"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let count = pool.elements();
        if count * 4 > distance.size.min(solid.size).min(out.size) {
            ctx.error(format!(
                "Liquid Cells: a pool of {} tiles is larger than its arrays",
                pool.pool_tiles
            ));
            return;
        }
        let uniforms = CellsUniforms {
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
                    buffer: distance,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: solid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: out,
                    offset: 0,
                },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.liquid_cells",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
