//! Uses the search-radius ratio (1.5 radii) from FLIP Fluids particlemesher.cpp `_searchRadiusFactor` (MIT); see THIRD_PARTY_NOTICES.md.
//! `node.particle_volume` — the liquid level set on the display's world
//! tiles: one value per node, the blended-sphere distance (Zhu and Bridson
//! 2005) over the blobs sorted into the display cells around it. One workgroup
//! per tile slot walks the blobs round its tile once
//! (`shaders/particle_volume.wgsl`).

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuComputePipeline};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, table_entries};
use manifold_water_liquid::field_build::{
    BLOB_CENTRE_SMOOTHING, BLOB_MAX_STRETCH, BLOB_NEIGHBOUR_CELLS, WEIGHT_FADE_END,
    WEIGHT_FADE_START, display_stride, expected_bulk_neighbours,
};
use manifold_water_liquid::fluid_particles::{CellRange, FluidBlob};
use manifold_water_liquid::tile_pool::tile_pool;
use manifold_water_liquid::{PARTICLE_RADIUS_CELLS, float_param};

// The kernel after the world-tile read side.
static BODY: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "{READ_SIDE}\n{}",
        include_str!("shaders/particle_volume.wgsl")
    )
});

/// Field order identical to the shader's `Params`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    cell_size: f32,
    blob_reach: f32,
    pool_tiles: u32,
    entries: u32,
    window: f32,
    run: u32,
    fade_start: f32,
    fade_end: f32,
}

/// Most cells a node's search window may reach; the shader's window span is
/// sized for it.
const MAX_SUPPORT_CELLS: f32 = 20.0;

/// The largest blob radius, metres: Particle Scale times the largest particle
/// radius (`particle_radius` simulation cells).
fn blob_reach(particle_scale: f32, sim_cell_size: f32, particle_radius: f32) -> f32 {
    particle_scale * particle_radius * sim_cell_size
}

/// How far from the cell its particle is sorted into a blob reaches,
/// metres: the support (1.5 radii) of the most stretched blob, plus the
/// farthest node.shape_particle_blobs moves a centre off its particle.
fn search_reach(blob_reach: f32, sim_cell_size: f32) -> f32 {
    1.5 * blob_reach * BLOB_MAX_STRETCH
        + BLOB_CENTRE_SMOOTHING * BLOB_NEIGHBOUR_CELLS * sim_cell_size
}

/// Cells per run of the window walk: about one simulation cell's blobs, so a
/// run holds about the same blobs at every Surface Detail. A divisor of the
/// tile's 8, so runs never cross a tile.
fn walk_run(stride: u32) -> u32 {
    match stride.pow(3) {
        0..=1 => 1,
        2..=3 => 2,
        4..=7 => 4,
        _ => 8,
    }
}

/// The reach refused unless it is a length whose search reach `window` the
/// node's search window covers.
fn check_reach(blob_reach: f32, window: f32, cell_size: f32) -> Result<(), String> {
    if !(blob_reach.is_finite() && blob_reach > 0.0) {
        return Err(format!(
            "Particle Volume: blob_reach {blob_reach} is not a length"
        ));
    }
    let window_cells = window / cell_size;
    if window_cells.is_nan() || window_cells > MAX_SUPPORT_CELLS {
        return Err(format!(
            "Particle Volume: blob_reach {blob_reach} m reaches {window_cells} cells; a node searches at most {MAX_SUPPORT_CELLS}"
        ));
    }
    Ok(())
}

manifold_node_engine::primitive! {
    name: ParticleVolume,
    type_id: "node.particle_volume",
    purpose: "The liquid's level set on the display's world tiles: at each node (node p at (p - 1/2) cells, the low corner of cell p), blended ellipsoids (Zhu and Bridson 2005): each blob carries a shape matrix A (stretched along sheets, shrunk when alone) and a size, and over the blobs within R = 1.5 blob reaches (particle_scale × particle_radius simulation cells × sim_cell_size, the largest blob radius) of the node, weighted (1 - (|size · A (c - x)|/R)^2)^3, the blend runs on each blob's stretched offset q = A (c - x), radius r and gradient scale g = |A q|/|q|; the level set is (|q̄| - r̄)/ḡ, the blended ellipsoid's algebraic distance over its gradient, negative inside and Zhu and Bridson's |x - c̄| - r̄ for round blobs, so particles up to 2R apart blend into one body; a node no blob reaches reads three blob reaches. The same blend gives weights, how much the field smoothing moves each node: the weighted mean of the blobs' neighbour counts (counts, from node.shape_particle_blobs) faded from 27% to 54% of the bulk count particles_per_cell implies within a simulation cell, 1 in bulk water and 0 on thin sheets and drops; and velocity, three f32 per pool element (element e at 3e..3e+2), the weighted mean velocity of the blobs' particles (sorted) in metres per second. Both are 0 where no blob reaches, in a free slot and in the canonical element. Each blob visits the inclusive FLIP grid support box from floor((centre - R)/cell + 1/2) through floor((centre + R)/cell + 1/2)+1. The blobs arrive sorted into the display cells and a node reads the cells within R of it. A node inside a solid (solid is the solid's distance on the same tiles, negative inside) takes at least its depth in the solid less half a cell: water against a wall reaches half a cell into it, so its surface meets the face at an angle behind it instead of lying on it, and deeper in no node reads as inside the liquid. A free slot and the canonical element hold three cells, the far value.",
    inputs: {
        blobs: Array(FluidBlob) required,
        cell_ranges: Array(CellRange) required,
        solid: Array(f32) required,
        tiles: Array(u32) required,
        counts: Array(f32) required,
        sorted: Array(FluidParticle) required,
        cell_size: ScalarF32 optional,
        particle_scale: ScalarF32 optional,
        sim_cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
        particles_per_cell: ScalarF32 optional,
    },
    outputs: {
        levelset: Array(f32),
        weights: Array(f32),
        velocity: Array(f32),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("particle_scale", "Particle Scale", 3.0, 0.25, 8.0),
        float_param!("sim_cell_size", "Simulation Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("particle_radius", "Particle Radius", PARTICLE_RADIUS_CELLS, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
        float_param!("particles_per_cell", "Particles Per Simulation Cell", 8.0, 0.001, 1000.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire blobs and counts from node.shape_particle_blobs, sorted from the node.sort_particles_into_cells it read, and cell_ranges from the node.sort_particles_into_cells that sorted them over the display tiles (one range per pool element, or nothing runs, a named error), solid the display solid on the same tiles, tiles, pool_tiles and cell_size from node.surface_tiles. Wire particle_scale from the value the blobs read, so the reach follows Particle Scale live, and sim_cell_size from the solver's cell size; particle_radius is the largest particle radius in simulation cells (the default is every solver's). A blob reaches at most 20 display cells from its particle's cell, stretch and centre move included. It reads the blobs round each tile, so it is a graph fusion boundary. Chain the field smoothing on the same tiles after it, each step reading weights; velocity feeds the surface march. particles_per_cell is the simulation's particles per simulation cell (8 in the FLIP default).",
    examples: [],
    picker: { label: "Particle Volume", category: Atom },
    summary: "Turns liquid particles into a distance field on a grid, the step before the surface mesh is drawn.",
    category: Particles3D,
    role: Filter,
    aliases: ["level set", "signed distance", "liquid field", "scalar field"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        volume_pipeline: Option<GpuComputePipeline> = None,
    },
}

impl Primitive for ParticleVolume {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        let elements = inputs
            .iter()
            .find(|(name, _)| *name == "solid")
            .map(|&(_, n)| n)?;
        match port {
            "levelset" | "weights" => Some(elements),
            "velocity" => elements.checked_mul(3),
            _ => None,
        }
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
        let window = search_reach(reach, sim_cell_size);
        if let Err(error) = check_reach(reach, window, pool.cell_size) {
            ctx.error(error);
            return;
        }
        let stride = match display_stride("Particle Volume", sim_cell_size, pool.cell_size) {
            Ok(stride) => stride,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let particles_per_cell = ctx.scalar_or_param("particles_per_cell", 8.0);
        if !(particles_per_cell.is_finite() && particles_per_cell > 0.0) {
            ctx.error(format!(
                "Particle Volume: {particles_per_cell} particles per simulation cell is not a density"
            ));
            return;
        }
        let bulk = expected_bulk_neighbours(particles_per_cell);
        if self.volume_pipeline.is_none() {
            let gpu = ctx.gpu_encoder();
            self.volume_pipeline = Some(gpu.device.create_compute_pipeline(
                BODY.as_str(),
                "cs_main",
                "node.particle_volume",
            ));
        }
        let (
            Some(blobs),
            Some(ranges),
            Some(solid),
            Some(tiles),
            Some(counts),
            Some(sorted),
            Some(levelset),
            Some(weights),
            Some(velocity),
        ) = (
            ctx.inputs.array("blobs"),
            ctx.inputs.array("cell_ranges"),
            ctx.inputs.array("solid"),
            ctx.inputs.array("tiles"),
            ctx.inputs.array("counts"),
            ctx.inputs.array("sorted"),
            ctx.outputs.array("levelset"),
            ctx.outputs.array("weights"),
            ctx.outputs.array("velocity"),
        )
        else {
            return;
        };
        let count = pool.elements();
        if count * 4 > solid.size.min(levelset.size).min(weights.size)
            || count * 12 > velocity.size
            || count * std::mem::size_of::<CellRange>() as u64 > ranges.size
        {
            ctx.error(format!(
                "Particle Volume: a pool of {} tiles is larger than its arrays; cell_ranges, solid, the level set and weights each hold one value per pool element, velocity three",
                pool.pool_tiles
            ));
            return;
        }
        let slots = blobs.size / std::mem::size_of::<FluidBlob>() as u64;
        if counts.size / 4 < slots || sorted.size / (std::mem::size_of::<FluidParticle>() as u64) < slots {
            ctx.error(format!(
                "Particle Volume: {slots} blobs and fewer counts or sorted particles; each blob reads its slot of both"
            ));
            return;
        }
        let uniforms = Uniforms {
            cell_size: pool.cell_size,
            blob_reach: reach,
            pool_tiles: pool.pool_tiles,
            entries: table_entries(pool.pool_tiles),
            window,
            run: walk_run(stride),
            fade_start: WEIGHT_FADE_START * bulk,
            fade_end: WEIGHT_FADE_END * bulk,
        };
        let gpu = ctx.gpu_encoder();
        // One workgroup per slot, and one more for the canonical element.
        gpu.native_enc.dispatch_compute(
            self.volume_pipeline
                .as_ref()
                .expect("particle volume pipeline prepared"),
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
                GpuBinding::Buffer {
                    binding: 6,
                    buffer: counts,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 7,
                    buffer: sorted,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 8,
                    buffer: weights,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 9,
                    buffer: velocity,
                    offset: 0,
                },
            ],
            [pool.pool_tiles + 1, 1, 1],
            "node.particle_volume",
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

    #[test]
    fn a_reach_beyond_the_search_window_is_refused() {
        // 0.15 m is 2.4 cells of a 0.0625 m cell, 18.75 of a 0.008 m cell and
        // 21.4 of a 0.007 m cell.
        assert!(check_reach(0.1, 0.15, 0.0625).is_ok());
        assert!(check_reach(0.1, 0.15, 0.008).is_ok());
        assert!(check_reach(0.1, 0.15, 0.007).is_err());
        // Particle Scale 3 fits at every Surface Detail.
        let sim = 0.0625;
        let reach = blob_reach(3.0, sim, PARTICLE_RADIUS_CELLS);
        let window = search_reach(reach, sim);
        for stride in 1..=3 {
            assert!(check_reach(reach, window, sim / stride as f32).is_ok());
        }
        // The window covers the most stretched blob and the centre's move.
        assert!(window > 1.5 * reach * BLOB_MAX_STRETCH + 0.5 * sim);
        for bad in [0.0, -0.1, f32::NAN, f32::INFINITY] {
            assert!(check_reach(bad, 0.15, 0.0625).is_err(), "reach {bad}");
        }
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "particle_volume_gpu_tests.rs"]
mod gpu_tests;
