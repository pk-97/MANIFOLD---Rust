//! `node.lattice_curvature` — FLIP's curvature grid on the whitewater
//! distance, over the liquid step's world tiles. A per-element gather on the
//! codegen path.
//!
//! Ported from FLIP Fluids particlelevelset.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

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
struct CurvatureUniforms {
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
}

manifold_node_engine::primitive! {
    name: LatticeCurvature,
    type_id: "node.lattice_curvature",
    purpose: "Mean curvature of the whitewater distance, as FLIP's whitewater measures it: at a world cell whose distance and six face neighbours' distances all lie within 2 cells of the surface, central differences give the curvature in 1/m, clamped to ±1 per cell, and the cell is known; elsewhere the value is 0 and unknown, for node.extend_lattice to fill. Cells are the liquid step's world cells, read through its tiles; a cell in a tile the table does not hold reads the canonical distance, far from the surface.",
    inputs: {
        distance: Array(f32) required,
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
    composition_notes: "distance the whitewater step's surface distance, one float per pooled world cell, with tiles and pool_tiles from the same liquid step and cell_size the domain's. Chain node.extend_lattice three times after it, as FLIP extends its curvature three layers.",
    examples: [],
    summary: "Measures how sharply the liquid's surface bends at each grid cell, which is where wave crests throw foam.",
    category: Particles3D,
    role: Filter,
    aliases: ["curvature", "mean curvature", "surface bend"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/lattice_curvature_body.wgsl"),
    input_access: [BufferGather, BufferGather],
    wgsl_includes: [READ_SIDE, WHITEWATER_COMMON],
}

impl Primitive for LatticeCurvature {
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
        let pool = match tile_pool(ctx, "Lattice Curvature") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(distance), Some(tiles), Some(out)) = (
            ctx.inputs.array("distance"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let count = pool.elements();
        if count * 4 > distance.size || count * 8 > out.size {
            ctx.error(format!(
                "Lattice Curvature: a pool of {} tiles is larger than its arrays",
                pool.pool_tiles
            ));
            return;
        }
        let uniforms = CurvatureUniforms {
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
            "node.lattice_curvature",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
