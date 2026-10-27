//! `node.field_smooth_blend`: the water field build's weighted, leashed blend
//! after each smoothing step, over the nodes of a world-tile pool. A per-node
//! atom that fuses with the z smoothing pass feeding it.
use crate::field_build::{FIELD_BAND_CELLS, FIELD_SMOOTHNESS_MAX};
use crate::float_param;
use crate::tile_pool::tile_pool;
use manifold_gpu::GpuBinding;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::freeze::classify::FusedOutputCapacity;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use std::borrow::Cow;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    cell_size: f32,
    smoothness: f32,
    band_cells: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

manifold_node_engine::primitive! {
    name: FieldSmoothBlend,
    type_id: "node.field_smooth_blend",
    purpose: "Blend a smoothed scalar lattice back toward the field it was smoothed from, per node, and hold the result within a leash of a reference field. Node arrays on the world tiles of a pool: one element per pooled node, all five arrays read at the same pool index. cell_size is one lattice cell in metres. Where |reference| is at least band_cells cells the prior passes through unchanged. Elsewhere the output is prior + weight · (smoothed - prior), clamped to reference ± smoothness × FIELD_LEASH_CELLS (half a cell). The canonical element and free slots hold the canonical level set, 3 cells.",
    inputs: {
        smoothed: Array(f32) required,
        prior: Array(f32) required,
        weights: Array(f32) required,
        reference: Array(f32) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        smoothness: ScalarF32 optional, band_cells: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: { field: Array(f32), },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("smoothness", "Smoothness", 1.0, 0.0, FIELD_SMOOTHNESS_MAX),
        float_param!("band_cells", "Band Cells", FIELD_BAND_CELLS, 0.0, 16.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Place after the z node.smooth_lattice pass of each smoothing step: smoothed is that pass, prior is the field the step started from, reference is node.particle_volume's level set, weights come from node.surface_weights, all pooled node arrays on the same tiles, with tiles, pool_tiles and cell_size from the same world tiles (node.surface_tiles for the display). Smoothness 1 is the tuned surface (the field build's half-cell leash), 0 returns the reference, 2 doubles the leash; band defaults to the field build's constant.",
    examples: [],
    picker: { label: "Field Smooth Blend", category: Atom },
    summary: "Eases a smoothed liquid surface back toward the original where it is thin, and never lets it drift far from the true surface.",
    category: Particles3D,
    role: Filter,
    aliases: ["smoothing blend", "leashed smoothing", "weighted smoothing"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/field_smooth_blend_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "smoothed" },
    wgsl_includes: [READ_SIDE],
}

impl Primitive for FieldSmoothBlend {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "field")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "smoothed")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Field Smooth Blend") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let smoothness = ctx.scalar_or_param("smoothness", 1.0);
        let band_cells = ctx.scalar_or_param("band_cells", FIELD_BAND_CELLS);
        let (Some(smoothed), Some(prior), Some(weights), Some(reference), Some(tiles), Some(field)) = (
            ctx.inputs.array("smoothed"),
            ctx.inputs.array("prior"),
            ctx.inputs.array("weights"),
            ctx.inputs.array("reference"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("field"),
        ) else {
            let missing = [
                ("smoothed", ctx.inputs.array("smoothed").is_none()),
                ("prior", ctx.inputs.array("prior").is_none()),
                ("weights", ctx.inputs.array("weights").is_none()),
                ("reference", ctx.inputs.array("reference").is_none()),
                ("field", ctx.outputs.array("field").is_none()),
            ]
            .into_iter()
            .filter_map(|(name, absent)| absent.then_some(name))
            .collect::<Vec<_>>()
            .join(", ");
            ctx.error(format!("Field Smooth Blend: missing {missing}"));
            return;
        };
        let needed = pool.elements();
        let short = [
            ("smoothed", smoothed.size / 4),
            ("prior", prior.size / 4),
            ("weights", weights.size / 4),
            ("reference", reference.size / 4),
            ("field", field.size / 4),
        ]
        .into_iter()
        .find(|&(_, have)| have < needed);
        if let Some((name, have)) = short {
            ctx.error(format!(
                "Field Smooth Blend: {name} holds {have} values but a pool of {} tiles needs {needed}",
                pool.pool_tiles
            ));
            return;
        }
        let uniforms = Uniforms {
            cell_size: pool.cell_size,
            smoothness,
            band_cells,
            pool_tiles: pool.pool_tiles as f32,
            dispatch_count: needed as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
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
                    buffer: smoothed,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: prior,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: weights,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: reference,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 6,
                    buffer: field,
                    offset: 0,
                },
            ],
            [(needed as u32).div_ceil(256), 1, 1],
            "node.field_smooth_blend",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(crate) mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;
