//! `node.carry_liquid_into_solids` — the water field's last solid step when
//! the drawn surface should meet a wall flush. Inside a drawn solid the field
//! is carried in from the wall's surface for `carry_cells` display cells (air
//! beyond that); inside any other solid it is clipped to max(value, 0). With
//! `carry_cells` 0 the output is exactly that clip.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuComputePipeline};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::field_build::{FIELD_REDISTANCE_CELLS, SOLID_CARRY_CELLS};
use manifold_water_liquid::float_param;
use manifold_water_liquid::tile_pool::{TilePool, tile_pool};

// The kernel after the world-tile read side it indexes nodes through.
static BODY: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "{READ_SIDE}\n{}",
        include_str!("shaders/carry_liquid_into_solids.wgsl")
    )
});
const THREADS: u32 = 64;

/// Field order identical to the shader's `CarryParams`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct CarryUniforms {
    cell: f32,
    carry_depth: f32,
    air: f32,
    pool_tiles: u32,
    element_count: u32,
    has_drawn: u32,
    pad_a: u32,
    pad_b: u32,
}

/// The shader's constants from the node's inputs. The field cell is the
/// display cell; the carry depth and the air value are multiples of it.
fn carry_uniforms(
    pool: TilePool,
    carry_cells: f32,
    has_drawn: bool,
) -> Result<CarryUniforms, String> {
    if !carry_cells.is_finite() {
        return Err("Carry Liquid Into Solids: carry_cells must be finite".into());
    }
    let element_count = u32::try_from(pool.elements()).map_err(|_| {
        format!(
            "Carry Liquid Into Solids: a pool of {} tiles has more elements than an index can address",
            pool.pool_tiles
        )
    })?;
    Ok(CarryUniforms {
        cell: pool.cell_size,
        carry_depth: carry_cells.max(0.0) * pool.cell_size,
        air: FIELD_REDISTANCE_CELLS * pool.cell_size,
        pool_tiles: pool.pool_tiles,
        element_count,
        has_drawn: u32::from(has_drawn),
        pad_a: 0,
        pad_b: 0,
    })
}

manifold_node_engine::primitive! {
    name: CarryLiquidIntoSolids,
    type_id: "node.carry_liquid_into_solids",
    purpose: "Clip the liquid level set against its solid, carrying the water into the solids the scene draws so the drawn surface meets a wall flush. All arrays are node arrays on the display's world tiles (node p at (p - 1/2) cells, the low corner of cell p), so the solid at a level-set node is its own element. At each node with solid distance sd (negative inside): outside solids the value is kept. Inside a carried solid (drawn_solid negative at the node, or every solid when drawn_solid is unwired) with sd above -carry_cells display cells, the value is the level set sampled trilinearly at the solid's surface point along the solid's normal plus half a cell into the water; deeper it is air, FIELD_REDISTANCE_CELLS cells. Inside any other solid, and inside every solid when carry_cells is 0, the value is max(value, 0), the clip rule. The normal is the normalised central difference of solid across the node's six neighbours (a zero gradient clips); a neighbour or sample in a tile the table does not hold reads the canonical element. A free slot and the canonical element hold three cells, the far value.",
    inputs: {
        levelset: Array(f32) required,
        solid: Array(f32) required,
        drawn_solid: Array(f32) optional,
        tiles: Array(u32) required,
        carry_cells: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("carry_cells", "Carry Cells", SOLID_CARRY_CELLS, 0.0, 8.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Place after the smoothing chain. Wire levelset from the last smoothing pass, solid the display solid, and tiles, pool_tiles and cell_size from node.surface_tiles. drawn_solid is optional and on the same tiles as solid: negative inside the solids the scene draws. Unwired, every solid carries water; wired, only drawn solids do and the rest (invisible colliders) still clip. carry_cells 0 gives the plain clamp. levelset, solid, drawn_solid and the output each hold one value per pool element, or nothing runs, a named error. It reads the level set at arbitrary nodes, so it is a graph fusion boundary.",
    examples: [],
    picker: { label: "Carry Liquid Into Solids", category: Atom },
    summary: "Keeps the liquid surface out of walls and bodies, and lets the water reach the walls the scene draws so the surface meets them flush.",
    category: Particles3D,
    role: Filter,
    aliases: ["solid carry", "wall flush", "clamp liquid to solids", "level set carry"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        carry_pipeline: Option<GpuComputePipeline> = None,
    },
}

impl Primitive for CarryLiquidIntoSolids {
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
        if self.carry_pipeline.is_none() {
            let source = BODY.as_str();
            let gpu = ctx.gpu_encoder();
            self.carry_pipeline = Some(gpu.device.create_compute_pipeline(
                source,
                "cs_main",
                "node.carry_liquid_into_solids",
            ));
        }
        let pool = match tile_pool(ctx, "Carry Liquid Into Solids") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let (Some(levelset), Some(solid), Some(tiles), Some(out)) = (
            ctx.inputs.array("levelset"),
            ctx.inputs.array("solid"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        ) else {
            ctx.error("Carry Liquid Into Solids: levelset, solid and tiles must be wired and the output allocated");
            return;
        };
        let drawn = ctx.inputs.array("drawn_solid");
        let uniforms = match carry_uniforms(
            pool,
            ctx.scalar_or_param("carry_cells", SOLID_CARRY_CELLS),
            drawn.is_some(),
        ) {
            Ok(uniforms) => uniforms,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let elements = u64::from(uniforms.element_count);
        let smallest = [Some(levelset), Some(solid), Some(out), drawn]
            .into_iter()
            .flatten()
            .map(|buffer| buffer.size / 4)
            .min()
            .unwrap_or(0);
        if smallest < elements {
            ctx.error(format!(
                "Carry Liquid Into Solids: a pool of {} tiles needs {elements} values in levelset, solid, drawn_solid and the output; the smallest holds {smallest}",
                pool.pool_tiles
            ));
            return;
        }
        let bindings = [
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
                buffer: solid,
                offset: 0,
            },
            // Unwired: the solid stands in, never read because has_drawn is 0.
            GpuBinding::Buffer {
                binding: 3,
                buffer: drawn.unwrap_or(solid),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: tiles,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: out,
                offset: 0,
            },
        ];
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            self.carry_pipeline
                .as_ref()
                .expect("carry pipeline prepared"),
            &bindings,
            [uniforms.element_count.div_ceil(THREADS), 1, 1],
            "node.carry_liquid_into_solids",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composed_shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(&BODY).expect("carry shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("carry shader validates");
        assert!(
            module
                .entry_points
                .iter()
                .any(|point| point.name == "cs_main")
        );
        assert_eq!(std::mem::size_of::<CarryUniforms>(), 32);
    }

    #[test]
    fn distances_follow_the_display_cell_of_a_pool() {
        let pool = TilePool {
            pool_tiles: 12,
            cell_size: 0.1,
        };
        let u = carry_uniforms(pool, 2.0, false).unwrap();
        assert!((u.carry_depth - 0.2).abs() < 1e-6);
        assert!((u.air - FIELD_REDISTANCE_CELLS * 0.1).abs() < 1e-6);
        assert_eq!(u.cell, 0.1);
        assert_eq!(u.pool_tiles, 12);
        assert_eq!(u.element_count, 12 * 512 + 1);
        assert_eq!(u.has_drawn, 0);
        let drawn = carry_uniforms(pool, 0.0, true).unwrap();
        assert_eq!(drawn.carry_depth, 0.0);
        assert_eq!(drawn.has_drawn, 1);
        // A negative depth is no depth: the plain clip.
        assert_eq!(carry_uniforms(pool, -1.0, true).unwrap().carry_depth, 0.0);
    }

    #[test]
    fn a_non_finite_depth_or_an_unaddressable_pool_is_an_error() {
        let pool = TilePool {
            pool_tiles: 12,
            cell_size: 0.1,
        };
        assert!(carry_uniforms(pool, f32::NAN, false).is_err());
        assert!(carry_uniforms(pool, f32::INFINITY, false).is_err());
        let huge = TilePool {
            pool_tiles: 9_000_000,
            cell_size: 0.1,
        };
        assert!(carry_uniforms(huge, 2.0, false).is_err());
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "carry_liquid_into_solids_gpu_tests.rs"]
mod gpu_tests;
