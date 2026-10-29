//! `node.field_smooth_blend`: one step of the water field build's smoothing
//! over the nodes of a world-tile pool. A step moves each node toward the
//! mean of the field along the surface through it (the node and a ring of
//! six points square to the normal, all weights positive, so it can only
//! flatten), eases that by the node's weight and holds it within a leash of
//! the reference. One workgroup per tile slot stages the tile and stride + 1
//! nodes round it (`shaders/field_smooth_blend.wgsl`).
use crate::field_build::{
    FIELD_BAND_CELLS, FIELD_SMOOTHNESS_MAX, MAX_DISPLAY_STRIDE, display_stride,
};
use crate::float_param;
use crate::tile_pool::tile_pool;
use manifold_gpu::{GpuBinding, GpuComputePipeline};
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, table_entries};
use std::borrow::Cow;

/// The kernel after the world-tile read side, its staged region (side 8 + 2
/// · (stride + 1)) sized for `stride`.
fn body(stride: u32) -> String {
    let side = 8 + 2 * (stride + 1);
    let source = include_str!("shaders/field_smooth_blend.wgsl").replacen(
        "const FS_A_LEN: u32 = 4096u;",
        &format!("const FS_A_LEN: u32 = {}u;", side * side * side),
        1,
    );
    format!("{READ_SIDE}\n{source}")
}

/// Field order identical to the shader's `Params`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    cell_size: f32,
    smoothness: f32,
    band_cells: f32,
    pool_tiles: u32,
    entries: u32,
    stride: u32,
    sim_cell_size: f32,
    keep_sheets: u32,
}

manifold_node_engine::primitive! {
    name: FieldSmoothBlend,
    type_id: "node.field_smooth_blend",
    purpose: "One step of the water field's smoothing, per node of a world-tile pool: prior moved toward its mean along the surface through the node (the node at 1/4 and six trilinear samples at 1/8 each on a ring square to the normal, from central differences a simulation cell apart or, with Keep Sheets where that gradient is under half (a sheet's or a gap's middle), the direction the field bends most; every weight positive, so a step only flattens and a flat surface stays put), by the node's weight, and held within a leash of the reference. With Keep Sheets (water) a node inside with the field higher half a simulation cell either way along the normal, a sheet's middle, rises at most half way to zero, so a thin sheet stays whole; without it (Matter) a thin bump of stray particles rounds off into the surface instead of standing as a fin. Node arrays on the world tiles of a pool, one element per pooled node, all read at the same pool index. cell_size is one lattice cell in metres and sim_cell_size the simulation's cell, a whole number (1 to 3) of lattice cells; the ring's radius grows with it, so the smoothing reaches as far at every Surface Detail. Where |reference| is at least band_cells simulation cells the prior passes through unchanged. Elsewhere the output is prior + mix(1/2, 1, weight) · (mean - prior), clamped to reference ± mix(1, 2, weight) · smoothness × FIELD_LEASH_CELLS (half a simulation cell). A node in a tile the table does not hold reads prior's canonical element. The canonical element and free slots hold the far value, 3 cells.",
    inputs: {
        prior: Array(f32) required,
        weights: Array(f32) required,
        reference: Array(f32) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        sim_cell_size: ScalarF32 optional,
        smoothness: ScalarF32 optional, band_cells: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: { field: Array(f32), },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("sim_cell_size", "Simulation Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("smoothness", "Smoothness", 1.0, 0.0, FIELD_SMOOTHNESS_MAX),
        float_param!("band_cells", "Band Cells", FIELD_BAND_CELLS, 0.0, 16.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
        ParamDef { name: Cow::Borrowed("keep_sheets"), label: "Keep Sheets", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Chain FIELD_SMOOTH_STEPS (3) of these after node.particle_volume: prior is the field the step starts from (the splat, then the step before), reference is node.particle_volume's level set, weights come from node.particle_volume, all pooled node arrays on the same tiles, with tiles, pool_tiles and cell_size from the same world tiles (node.surface_tiles for the display) and sim_cell_size from the domain. Smoothness 1 is the tuned surface (the field build's half-cell leash), 0 returns the reference, 2 doubles the leash; band defaults to the field build's constant. It reads the nodes round each tile, so it is a graph fusion boundary.",
    examples: [],
    picker: { label: "Field Smooth Blend", category: Atom },
    summary: "Smooths a liquid surface one step, easing it back toward the original where it is thin, and never lets it drift far from the true surface.",
    category: Particles3D,
    role: Filter,
    aliases: ["smoothing blend", "leashed smoothing", "weighted smoothing", "smooth lattice"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        smooth_pipelines: [Option<GpuComputePipeline>; MAX_DISPLAY_STRIDE as usize] = Default::default(),
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
        let sim_cell_size = ctx.scalar_or_param("sim_cell_size", 0.0625);
        let stride = match display_stride("Field Smooth Blend", sim_cell_size, pool.cell_size) {
            Ok(stride) => stride,
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
            stride,
            sim_cell_size,
            keep_sheets: u32::from(!matches!(
                ctx.params.get("keep_sheets"),
                Some(ParamValue::Bool(false))
            )),
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = self.smooth_pipelines[stride as usize - 1].get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(&body(stride), "cs_main", "node.field_smooth_blend")
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
        for stride in 1..=MAX_DISPLAY_STRIDE {
            let source = body(stride);
            let module = naga::front::wgsl::parse_str(&source)
                .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&source)));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap_or_else(|error| panic!("{error:?}"));
        }
        assert!(
            body(3).contains("FS_A_LEN: u32 = 4096u") && body(1).contains("FS_A_LEN: u32 = 1728u")
        );
        assert_eq!(std::mem::size_of::<Uniforms>(), 32);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(crate) mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;
