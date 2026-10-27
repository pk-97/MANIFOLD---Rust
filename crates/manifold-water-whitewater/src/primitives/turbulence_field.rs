//! FLIP cell-centre turbulence from the MAC velocity field on the liquid
//! step's world tiles; per-element gather. Ported from FLIP Fluids turbulencefield.cpp (MIT); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::float_param;
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::grid::LIQUID_FACES;
use manifold_water_liquid::tile_pool::tile_pool;
use manifold_water_liquid::whitewater::WHITEWATER_COMMON;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TurbulenceUniforms {
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
}

manifold_node_engine::primitive! {
    name: TurbulenceField,
    type_id: "node.turbulence_field",
    purpose: "FLIP turbulence at each liquid world cell: sum cell-centre MAC velocity differences over the asymmetric i-2 through i+1 window, weighted by direction and distance within sqrt(12) cells. Air cells are zero. Distance and the step's pooled faces are read through its world-tile table; a cell in a tile the table does not hold reads the canonical element (air, a face with no weight, so no velocity).",
    inputs: {
        distance: Array(f32) required,
        faces: Array(FaceSample) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Feed the liquid distance (one float per pooled world cell) and the step's pooled faces, with tiles and pool_tiles from the same liquid step; inside_turbulence_potential samples this field for bubble emission. The field materializes before particle sampling.",
    examples: [],
    summary: "Measures local liquid agitation for whitewater emission.",
    category: Particles3D,
    role: Filter,
    aliases: ["turbulence", "liquid agitation"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/turbulence_field_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather],
    output_capacity: manifold_node_engine::freeze::classify::FusedOutputCapacity::FromInput { input: "distance" },
    wgsl_includes: [READ_SIDE, WHITEWATER_COMMON, LIQUID_FACES],
}

impl Primitive for TurbulenceField {
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
        let pool = match tile_pool(ctx, "Turbulence Field") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(distance), Some(faces), Some(tiles), Some(out)) = (
            ctx.inputs.array("distance"),
            ctx.inputs.array("faces"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let count = pool.elements();
        if count * 4 > distance.size.min(out.size)
            || count * std::mem::size_of::<FaceSample>() as u64 > faces.size
        {
            ctx.error(format!(
                "Turbulence Field: a pool of {} tiles is larger than its arrays",
                pool.pool_tiles
            ));
            return;
        }
        let uniforms = TurbulenceUniforms {
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
                    buffer: faces,
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
            "node.turbulence_field",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
