//! `node.surface_weights` — per node of the display's world tiles, how much
//! smoothing the water surface gets there and the water's velocity there. The
//! weight is 1 in bulk water and 0 on thin sheets and drops, read from how
//! many neighbours the nearby particles have; the constants live in
//! `manifold_water_liquid::field_build`.

use manifold_gpu::BufferUse;
use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::field_build::{
    FIELD_BAND_CELLS, NEIGHBOUR_COUNT_RADIUS_CELLS, WEIGHT_CARRY_RADIUS_CELLS, WEIGHT_FADE_END,
    WEIGHT_FADE_START, expected_bulk_neighbours,
};
use manifold_water_liquid::float_param;
use manifold_water_liquid::fluid_particles::CellRange;
use manifold_water_liquid::tile_pool::{TilePool, tile_pool};

// The kernel after the world-tile read side it indexes cells through.
static SHADER: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "{READ_SIDE}\n{}",
        include_str!("shaders/surface_weights.wgsl")
    )
});
const THREADS: u32 = 64;

/// Field order identical to the shader's `Params`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct WeightUniforms {
    cell: f32,
    count_reach: f32,
    carry_reach: f32,
    band: f32,
    fade_start: f32,
    fade_end: f32,
    count_cells: f32,
    carry_cells: f32,
    pool_tiles: u32,
    particle_capacity: u32,
    element_count: u32,
    pad: u32,
}

/// The shader's constants from the node's inputs. The field cell is the
/// display cell, and every reach is a multiple of it from `field_build`.
fn weight_uniforms(
    pool: TilePool,
    particles_per_cell: f32,
    sim_cell_size: f32,
    particle_capacity: u32,
) -> Result<WeightUniforms, String> {
    if !(particles_per_cell.is_finite() && particles_per_cell > 0.0) {
        return Err("Surface Weights: particles_per_cell must be finite and positive".into());
    }
    if !(sim_cell_size.is_finite() && sim_cell_size > 0.0) {
        return Err("Surface Weights: sim_cell_size must be finite and positive".into());
    }
    let element_count = u32::try_from(pool.elements())
        .ok()
        .filter(|&n| n.checked_mul(3).is_some())
        .ok_or_else(|| {
            format!(
                "Surface Weights: a pool of {} tiles has more elements than a velocity index can address",
                pool.pool_tiles
            )
        })?;
    let cell = pool.cell_size;
    // The density is per simulation cell; scale it to this field cell's volume.
    let per_field_cell = particles_per_cell * (cell / sim_cell_size).powi(3);
    let bulk = expected_bulk_neighbours(per_field_cell);
    Ok(WeightUniforms {
        cell,
        count_reach: NEIGHBOUR_COUNT_RADIUS_CELLS * cell,
        carry_reach: WEIGHT_CARRY_RADIUS_CELLS * cell,
        band: FIELD_BAND_CELLS * cell,
        fade_start: WEIGHT_FADE_START * bulk,
        fade_end: WEIGHT_FADE_END * bulk,
        count_cells: NEIGHBOUR_COUNT_RADIUS_CELLS,
        carry_cells: WEIGHT_CARRY_RADIUS_CELLS,
        pool_tiles: pool.pool_tiles,
        particle_capacity,
        element_count,
        pad: 0,
    })
}

/// (Re)allocate `slot` when its size changes; never otherwise.
fn ensure_buffer(
    slot: &mut Option<GpuBuffer>,
    device: &manifold_gpu::GpuDevice,
    bytes: u64,
) -> Result<GpuBuffer, String> {
    let bytes = bytes.max(16);
    if slot.as_ref().is_none_or(|buffer| buffer.size != bytes) {
        *slot = Some(
            device
                .try_create_buffer(bytes, BufferUse::Gpu)
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(slot.as_ref().expect("buffer was just allocated").clone())
}

manifold_node_engine::primitive! {
    name: SurfaceWeights,
    type_id: "node.surface_weights",
    purpose: "Per node of the display's world tiles, how much smoothing the surface gets and the water's velocity there. weights holds one f32 per pool element: 1 in bulk water, fading to 0 on thin sheets and drops, from how many neighbours the particles near the node have (each particle counts the live particles within 1.5 display cells, the nodes average those counts over the particles within 2 display cells with a poly6 kernel, and the mean fades from 18% to 36% of the bulk count that particles_per_cell implies for one display cell). velocity holds three f32 per pool element (x, y, z, element-major, element e at 3e..3e+2), the same kernel's mean particle velocity in metres per second. Node p is at (p - 1/2) cells, the low corner of cell p, and reference holds one distance per node. Both are zero at nodes outside the field's band (|reference| at least 4 display cells), where no particle is within reach, in a free slot and in the canonical element. A pure function of this frame's particles: no history, no frame counter.",
    inputs: {
        particles: Array(FluidParticle) required,
        cell_ranges: Array(CellRange) required,
        reference: Array(f32) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        sim_cell_size: ScalarF32 optional,
        particles_per_cell: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        weights: Array(f32),
        velocity: Array(f32),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("sim_cell_size", "Simulation Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("particles_per_cell", "Particles Per Simulation Cell", 8.0, 0.001, 1000.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire particles and cell_ranges from one node.sort_particles_into_cells over the display tiles (sorted and cell_ranges, one range per pool element or nothing runs, a named error), reference from node.particle_volume's level set on the same tiles, and tiles, pool_tiles and cell_size from node.surface_tiles (cell_size is the display cell the sort binned by). particles_per_cell is the simulation's particles per simulation cell (8 in the FLIP default) and sim_cell_size that cell's side in metres (wire it from the simulation's cell size); the node scales the density to the display cell, so the bulk count follows Surface Detail. Feed weights and velocity to the field smoothing and the surface march. Two compute passes in order (counts, then weights) with an owned output buffer, so it is a graph fusion boundary.",
    examples: [],
    picker: { label: "Surface Weights", category: Atom },
    summary: "Works out where the water surface should be smoothed (bulk water) and left alone (thin sheets, drops), and how fast the water moves there.",
    category: Particles3D,
    role: Filter,
    aliases: ["surface weights", "smoothing weights", "velocity lattice"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        counts_pipeline: Option<GpuComputePipeline> = None,
        weights_pipeline: Option<GpuComputePipeline> = None,
        counts: Option<GpuBuffer> = None,
        weights: Option<GpuBuffer> = None,
        velocity: Option<GpuBuffer> = None,
    },
}

impl Primitive for SurfaceWeights {
    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "weights" | "velocity")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "weights" => self.weights.as_ref(),
            "velocity" => self.velocity.as_ref(),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        let elements = inputs
            .iter()
            .find(|(name, _)| *name == "reference")
            .map(|&(_, n)| n)?;
        match port {
            "weights" => Some(elements),
            "velocity" => Some(elements.saturating_mul(3)),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        {
            let source = SHADER.as_str();
            let gpu = ctx.gpu_encoder();
            if self.counts_pipeline.is_none() {
                self.counts_pipeline = Some(gpu.device.create_compute_pipeline(
                    source,
                    "cs_counts",
                    "node.surface_weights",
                ));
            }
            if self.weights_pipeline.is_none() {
                self.weights_pipeline = Some(gpu.device.create_compute_pipeline(
                    source,
                    "cs_weights",
                    "node.surface_weights",
                ));
            }
        }
        let pool = match tile_pool(ctx, "Surface Weights") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let (Some(particles), Some(ranges), Some(reference), Some(tiles)) = (
            ctx.inputs.array("particles"),
            ctx.inputs.array("cell_ranges"),
            ctx.inputs.array("reference"),
            ctx.inputs.array("tiles"),
        ) else {
            ctx.error(
                "Surface Weights: particles, cell_ranges, reference and tiles must all be wired",
            );
            return;
        };
        let capacity = u32::try_from(particles.size / std::mem::size_of::<FluidParticle>() as u64)
            .unwrap_or(u32::MAX);
        let uniforms = match weight_uniforms(
            pool,
            ctx.scalar_or_param("particles_per_cell", 8.0),
            ctx.scalar_or_param("sim_cell_size", 0.0625),
            capacity,
        ) {
            Ok(uniforms) => uniforms,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let elements = u64::from(uniforms.element_count);
        if reference.size / 4 < elements
            || ranges.size / (std::mem::size_of::<CellRange>() as u64) < elements
        {
            ctx.error(format!(
                "Surface Weights: reference holds {} values and cell_ranges {} ranges; a pool of {} tiles needs {elements} of each",
                reference.size / 4,
                ranges.size / std::mem::size_of::<CellRange>() as u64,
                pool.pool_tiles
            ));
            return;
        }

        let buffers = {
            let gpu = ctx.gpu_encoder();
            let counts = ensure_buffer(&mut self.counts, gpu.device, u64::from(capacity) * 4);
            let weights = ensure_buffer(&mut self.weights, gpu.device, elements * 4);
            let velocity = ensure_buffer(&mut self.velocity, gpu.device, elements * 12);
            counts.and_then(|c| weights.and_then(|w| velocity.map(|v| (c, w, v))))
        };
        let (counts, weights, velocity) = match buffers {
            Ok(buffers) => buffers,
            Err(error) => {
                ctx.error(format!(
                    "Surface Weights: output allocation failed: {error}"
                ));
                return;
            }
        };

        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: particles,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: ranges,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: reference,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &counts,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: &weights,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: &velocity,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 7,
                buffer: tiles,
                offset: 0,
            },
        ];
        let gpu = ctx.gpu_encoder();
        if capacity > 0 {
            gpu.native_enc.dispatch_compute(
                self.counts_pipeline
                    .as_ref()
                    .expect("counts pipeline prepared"),
                &bindings,
                [capacity.div_ceil(THREADS), 1, 1],
                "node.surface_weights.counts",
            );
            gpu.native_enc.compute_memory_barrier_buffers();
        }
        gpu.native_enc.dispatch_compute(
            self.weights_pipeline
                .as_ref()
                .expect("weights pipeline prepared"),
            &bindings,
            [uniforms.element_count.div_ceil(THREADS), 1, 1],
            "node.surface_weights.weights",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(pool_tiles: u32, cell_size: f32) -> TilePool {
        TilePool {
            pool_tiles,
            cell_size,
        }
    }

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(&SHADER).expect("surface weights shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("surface weights shader validates");
        for entry in ["cs_counts", "cs_weights"] {
            assert!(
                module.entry_points.iter().any(|point| point.name == entry),
                "missing {entry}"
            );
        }
        assert_eq!(std::mem::size_of::<WeightUniforms>(), 48);
    }

    #[test]
    fn uniforms_follow_the_display_cell_of_a_pool() {
        let u = weight_uniforms(pool(12, 0.1), 8.0, 0.1, 1000).unwrap();
        let bulk = expected_bulk_neighbours(8.0);
        assert_eq!(u.cell, 0.1);
        assert_eq!(u.count_reach, NEIGHBOUR_COUNT_RADIUS_CELLS * 0.1);
        assert_eq!(u.carry_reach, WEIGHT_CARRY_RADIUS_CELLS * 0.1);
        assert_eq!(u.band, FIELD_BAND_CELLS * 0.1);
        assert_eq!(u.count_cells, NEIGHBOUR_COUNT_RADIUS_CELLS);
        assert_eq!(u.carry_cells, WEIGHT_CARRY_RADIUS_CELLS);
        assert_eq!(u.fade_start, WEIGHT_FADE_START * bulk);
        assert_eq!(u.fade_end, WEIGHT_FADE_END * bulk);
        assert_eq!(u.pool_tiles, 12);
        assert_eq!(u.element_count, 12 * 512 + 1);
        assert_eq!(u.particle_capacity, 1000);
    }

    #[test]
    fn bulk_count_follows_the_display_cell() {
        // Display cell 0.0625. A simulation cell of the same size keeps 8 per
        // cell; one four times as wide holds 64 times as many display-cell
        // volumes, so 8 per cell is 8 / 64 per display cell.
        let at = |sim_cell: f32| weight_uniforms(pool(3, 0.0625), 8.0, sim_cell, 10).unwrap();
        assert_eq!(
            at(0.0625).fade_start,
            WEIGHT_FADE_START * expected_bulk_neighbours(8.0)
        );
        assert_eq!(
            at(0.25).fade_start,
            WEIGHT_FADE_START * expected_bulk_neighbours(8.0 / 64.0)
        );
        assert_eq!(
            at(0.25).fade_end,
            WEIGHT_FADE_END * expected_bulk_neighbours(8.0 / 64.0)
        );
    }

    #[test]
    fn a_bad_density_or_pool_is_an_error() {
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(weight_uniforms(pool(3, 0.0625), bad, 0.0625, 10).is_err());
            assert!(weight_uniforms(pool(3, 0.0625), 8.0, bad, 10).is_err());
        }
        // A velocity index (three per element) must fit in 32 bits.
        assert!(weight_uniforms(pool(9_000_000, 0.0625), 8.0, 0.0625, 10).is_err());
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "surface_weights_gpu_tests.rs"]
mod gpu_tests;
