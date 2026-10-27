//! `node.field_smooth_blend`: one step of the water field build's smoothing
//! over the nodes of a world-tile pool. A step smooths the field with the
//! [1 2 1] / 4 filter along x, y and z, eases the result back toward the
//! field by the node's weight and holds it within a leash of the reference.
//! One workgroup per tile slot stages the tile and one node round it
//! (`shaders/field_smooth_blend.wgsl`).
use crate::field_build::{FIELD_BAND_CELLS, FIELD_SMOOTHNESS_MAX};
use crate::float_param;
use crate::tile_pool::tile_pool;
use manifold_gpu::{GpuBinding, GpuComputePipeline};
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, table_entries};
use std::borrow::Cow;

// The kernel after the world-tile read side.
static BODY: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "{READ_SIDE}\n{}",
        include_str!("shaders/field_smooth_blend.wgsl")
    )
});

/// Field order identical to the shader's `Params`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    cell_size: f32,
    smoothness: f32,
    band_cells: f32,
    pool_tiles: u32,
    entries: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

manifold_node_engine::primitive! {
    name: FieldSmoothBlend,
    type_id: "node.field_smooth_blend",
    purpose: "One step of the water field's smoothing, per node of a world-tile pool: prior smoothed with the [1, 2, 1] / 4 filter along x, then y, then z, blended back toward prior by the node's weight and held within a leash of the reference. Node arrays on the world tiles of a pool, one element per pooled node, all read at the same pool index. cell_size is one lattice cell in metres. Where |reference| is at least band_cells cells the prior passes through unchanged. Elsewhere the output is prior + weight · (smoothed - prior), clamped to reference ± smoothness × FIELD_LEASH_CELLS (half a cell). A node in a tile the table does not hold reads prior's canonical element, and each filter pass leaves it at the far value, 3 cells. The canonical element and free slots hold the far value.",
    inputs: {
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
    composition_notes: "Chain FIELD_SMOOTH_STEPS (3) of these after node.particle_volume: prior is the field the step starts from (the splat, then the step before), reference is node.particle_volume's level set, weights come from node.surface_weights, all pooled node arrays on the same tiles, with tiles, pool_tiles and cell_size from the same world tiles (node.surface_tiles for the display). Smoothness 1 is the tuned surface (the field build's half-cell leash), 0 returns the reference, 2 doubles the leash; band defaults to the field build's constant. It reads the nodes round each tile, so it is a graph fusion boundary.",
    examples: [],
    picker: { label: "Field Smooth Blend", category: Atom },
    summary: "Smooths a liquid surface one step, easing it back toward the original where it is thin, and never lets it drift far from the true surface.",
    category: Particles3D,
    role: Filter,
    aliases: ["smoothing blend", "leashed smoothing", "weighted smoothing", "smooth lattice"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        smooth_pipeline: Option<GpuComputePipeline> = None,
    },
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
                    .find(|(name, _)| *name == "prior")
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
        let (Some(prior), Some(weights), Some(reference), Some(tiles), Some(field)) = (
            ctx.inputs.array("prior"),
            ctx.inputs.array("weights"),
            ctx.inputs.array("reference"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("field"),
        ) else {
            let missing = [
                ("prior", ctx.inputs.array("prior").is_none()),
                ("weights", ctx.inputs.array("weights").is_none()),
                ("reference", ctx.inputs.array("reference").is_none()),
                ("tiles", ctx.inputs.array("tiles").is_none()),
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
            pool_tiles: pool.pool_tiles,
            entries: table_entries(pool.pool_tiles),
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = self.smooth_pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(BODY.as_str(), "cs_main", "node.field_smooth_blend")
        });
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
                buffer(1, prior),
                buffer(2, weights),
                buffer(3, reference),
                buffer(4, tiles),
                buffer(5, field),
            ],
            [pool.pool_tiles + 1, 1, 1],
            "node.field_smooth_blend",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
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
pub(crate) mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;
