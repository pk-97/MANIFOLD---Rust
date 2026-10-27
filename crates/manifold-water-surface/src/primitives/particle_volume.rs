//! Uses the search-radius ratio (1.5 radii) from FLIP Fluids particlemesher.cpp `_searchRadiusFactor` (MIT); see THIRD_PARTY_NOTICES.md.
//! `node.particle_volume` — the liquid level set on the display's world
//! tiles: one value per node, the distance to the nearest spherical kernel
//! among the blobs sorted into the display cells around it. A per-element
//! gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::fluid_particles::{CellRange, FluidBlob};
use manifold_water_liquid::tile_pool::tile_pool;
use manifold_water_liquid::{PARTICLE_RADIUS_CELLS, float_param};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VolumeUniforms {
    cell_size: f32,
    particle_scale: f32,
    sim_cell_size: f32,
    particle_radius: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

/// Most cells of kernel support (1.5 radii) a node's search window may hold;
/// the shader's window span is sized for it.
const MAX_SUPPORT_CELLS: f32 = 16.0;

/// The largest blob radius, metres: Particle Scale times the largest particle
/// radius (`particle_radius` simulation cells).
fn blob_reach(particle_scale: f32, sim_cell_size: f32, particle_radius: f32) -> f32 {
    particle_scale * particle_radius * sim_cell_size
}

/// The reach refused unless it is a length whose support the search window
/// covers.
fn check_reach(blob_reach: f32, cell_size: f32) -> Result<(), String> {
    if !(blob_reach.is_finite() && blob_reach > 0.0) {
        return Err(format!(
            "Particle Volume: blob_reach {blob_reach} is not a length"
        ));
    }
    let support_cells = 1.5 * blob_reach / cell_size;
    if support_cells > MAX_SUPPORT_CELLS {
        return Err(format!(
            "Particle Volume: blob_reach {blob_reach} m is {support_cells} cells of support; a node searches at most {MAX_SUPPORT_CELLS}"
        ));
    }
    Ok(())
}

manifold_node_engine::primitive! {
    name: ParticleVolume,
    type_id: "node.particle_volume",
    purpose: "The liquid's level set on the display's world tiles: at each node (node p at (p - 1/2) cells, the low corner of cell p), the distance to the nearest kernel sphere, |x - c| - r, negative inside and initialized to three times the blob reach (particle_scale × particle_radius simulation cells × sim_cell_size, the largest blob radius); each kernel visits the inclusive FLIP grid support box from floor((centre - 1.5r)/cell + 1/2) through floor((centre + 1.5r)/cell + 1/2)+1. The blobs arrive sorted into the display cells and a node reads the cells within 1.5 reaches of it; a larger blob is cut where its support leaves the searched cells. Nodes inside a solid (solid is a node array on the same tiles) are never inside the liquid; borders follow the same solid-SDF rule as the production FLIP mesher (the preview-only +0.001 closure is not applied). A free slot and the canonical element hold three cells, the far value.",
    inputs: {
        blobs: Array(FluidBlob) required,
        cell_ranges: Array(CellRange) required,
        solid: Array(f32) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        particle_scale: ScalarF32 optional,
        sim_cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        levelset: Array(f32),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("particle_scale", "Particle Scale", 3.0, 0.25, 8.0),
        float_param!("sim_cell_size", "Simulation Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("particle_radius", "Particle Radius", PARTICLE_RADIUS_CELLS, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire blobs from node.shape_particle_blobs and cell_ranges from the node.sort_particles_into_cells that sorted them over the display tiles (one range per pool element, or nothing runs, a named error), solid the display solid on the same tiles, tiles, pool_tiles and cell_size from node.surface_tiles. Wire particle_scale from the value the blobs read, so the reach follows Particle Scale live, and sim_cell_size from the solver's cell size; particle_radius is the largest particle radius in simulation cells (the default is every solver's). The reach is at most 16 display cells of support. Chain node.redistance_lattice and the field smoothing on the same tiles after it.",
    examples: [],
    picker: { label: "Particle Volume", category: Atom },
    summary: "Turns liquid particles into a distance field on a grid, the step before the surface mesh is drawn.",
    category: Particles3D,
    role: Filter,
    aliases: ["level set", "signed distance", "liquid field", "scalar field"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/particle_volume_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: manifold_node_engine::freeze::classify::FusedOutputCapacity::FromInput { input: "solid" },
    wgsl_includes: [READ_SIDE],
}

impl Primitive for ParticleVolume {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "levelset")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "solid")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Particle Volume") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let particle_scale = ctx.scalar_or_param("particle_scale", 3.0);
        let sim_cell_size = ctx.scalar_or_param("sim_cell_size", 0.0625);
        let particle_radius = ctx.scalar_or_param("particle_radius", PARTICLE_RADIUS_CELLS);
        let reach = blob_reach(particle_scale, sim_cell_size, particle_radius);
        if let Err(error) = check_reach(reach, pool.cell_size) {
            ctx.error(error);
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(blobs), Some(ranges), Some(solid), Some(tiles), Some(levelset)) = (
            ctx.inputs.array("blobs"),
            ctx.inputs.array("cell_ranges"),
            ctx.inputs.array("solid"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("levelset"),
        ) else {
            return;
        };
        let count = pool.elements();
        if count * 4 > solid.size.min(levelset.size)
            || count * std::mem::size_of::<CellRange>() as u64 > ranges.size
        {
            ctx.error(format!(
                "Particle Volume: a pool of {} tiles is larger than its arrays; cell_ranges, solid and the level set each hold one value per pool element",
                pool.pool_tiles
            ));
            return;
        }
        let uniforms = VolumeUniforms {
            cell_size: pool.cell_size,
            particle_scale,
            sim_cell_size,
            particle_radius,
            pool_tiles: pool.pool_tiles as f32,
            dispatch_count: count as u32,
            _pad0: 0,
            _pad1: 0,
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
                    buffer: blobs,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: ranges,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: solid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: levelset,
                    offset: 0,
                },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.particle_volume",
        );
    }
}

#[cfg(test)]
mod cpu_tests {
    use super::*;

    #[test]
    fn particle_volume_codegen_validates() {
        let wgsl =
            manifold_node_engine::freeze::codegen::standalone_for_spec::<super::ParticleVolume>()
                .expect("particle_volume buffer codegen");
        let module = naga::front::wgsl::parse_str(&wgsl)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|error| panic!("{error:?}"));
    }

    #[test]
    fn a_reach_beyond_the_search_window_is_refused() {
        // 1.5 * 0.1 m is 2.4 cells of a 0.0625 m cell and 15 of a 0.01 m cell.
        assert!(check_reach(0.1, 0.0625).is_ok());
        assert!(check_reach(0.1, 0.01).is_ok());
        assert!(check_reach(0.1, 0.009).is_err());
        assert!(check_reach(blob_reach(3.0, 0.0625, PARTICLE_RADIUS_CELLS), 0.0625 / 4.0).is_ok());
        for bad in [0.0, -0.1, f32::NAN, f32::INFINITY] {
            assert!(check_reach(bad, 0.0625).is_err(), "reach {bad}");
        }
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "particle_volume_gpu_tests.rs"]
mod gpu_tests;
