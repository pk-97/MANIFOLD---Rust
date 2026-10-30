//! `node.shape_particle_blobs` — one surface blob per sorted liquid particle,
//! shaped by the particles within a simulation cell of it (Yu and Turk 2013):
//! its centre smoothed toward theirs, stretched along the sheet or strand
//! they lie in, and shrunk toward the particle's own radius when it is alone
//! (`shaders/shape_particle_blobs.wgsl`).

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuComputePipeline};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::float_param;
use manifold_water_liquid::fluid_particles::{CellRange, FluidBlob};
use manifold_water_liquid::tile_pool::tile_pool;

// The kernel after the world-tile read side.
static BODY: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "{READ_SIDE}\n{}",
        include_str!("shaders/shape_particle_blobs.wgsl")
    )
});

/// Field order identical to the shader's `Params`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    cell_size: f32,
    sim_cell_size: f32,
    particle_scale: f32,
    count: u32,
    pool_tiles: u32,
    _pad_h: u32,
    _pad0: u32,
    _pad1: u32,
}

manifold_node_engine::primitive! {
    name: ShapeParticleBlobs,
    type_id: "node.shape_particle_blobs",
    purpose: "Give each sorted liquid particle an ellipsoidal surface blob shaped by its neighbours (Yu and Turk 2013): the particles within one sim_cell_size of it, weighted 1 - (d / sim_cell_size)^3, found through the display cells they are sorted into. The blob's centre moves 0.9 of the way to their weighted mean; its axes follow their weighted spread, the shortest at least a quarter of the longest, at the round blob's volume, so blobs in a sheet flatten into it; with fewer than 3 neighbours the stretch and smoothing fade out. A blob with no other particle within its blend radius (1.5 round radii, node.particle_volume's) shrinks from particle_scale × the particle's radius toward the radius itself, halfway with one, so lone drops draw at their true size while a thinned sheet keeps its size. The stretch is at most 2. A lone drop's semi-axes are at least 0.75 display cells, a joined blob's 0.75 sim_cell_size (or its round radius if smaller), so sheets keep their thickness at every Surface Detail. Each blob holds its centre, its round radius r and the symmetric shape matrix A: a point at offset y from the centre is inside when |A y| < r. counts holds each slot's neighbours within the simulation cell, which node.particle_volume blends into how much each node is smoothed. An inactive particle (radius 0) gives a blob of radius 0 and a count of 0.",
    inputs: {
        sorted: Array(FluidParticle) required,
        cell_ranges: Array(CellRange) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        sim_cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
        particle_scale: ScalarF32 optional,
    },
    outputs: {
        blobs: Array(FluidBlob),
        counts: Array(f32),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("sim_cell_size", "Simulation Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
        float_param!("particle_scale", "Particle Scale", 3.0, 0.25, 8.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Feed it the sorted particles and cell_ranges from node.sort_particles_into_cells, with tiles, pool_tiles and cell_size from the node.surface_tiles it sorted over and sim_cell_size from the solver. Wire its blobs and counts to node.particle_volume, and the same Particle Scale value into both. Particle Scale is live: changing it reshapes the next frame's surface without touching the simulation. It reads each particle's neighbours, so it is a graph fusion boundary.",
    examples: [],
    picker: { label: "Shape Particle Blobs", category: Atom },
    summary: "Turns each liquid particle into a blob shaped by its neighbours, flat in sheets and small when alone, which the liquid surface is built from.",
    category: Particles3D,
    role: Map,
    aliases: ["particle spheres", "liquid blobs", "particle kernels", "anisotropic kernels"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        shape_pipeline: Option<GpuComputePipeline> = None,
    },
}

impl Primitive for ShapeParticleBlobs {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        matches!(port, "blobs" | "counts")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "sorted")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Shape Particle Blobs") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let particle_scale = ctx.scalar_or_param("particle_scale", 3.0);
        let sim_cell_size = ctx.scalar_or_param("sim_cell_size", 0.0625);
        if !(sim_cell_size > 0.0 && sim_cell_size.is_finite()) {
            ctx.error(format!(
                "Shape Particle Blobs: simulation cell {sim_cell_size} is not a length"
            ));
            return;
        }
        if self.shape_pipeline.is_none() {
            let gpu = ctx.gpu_encoder();
            self.shape_pipeline = Some(gpu.device.create_compute_pipeline(
                BODY.as_str(),
                "cs_main",
                "node.shape_particle_blobs",
            ));
        }
        let (Some(sorted), Some(ranges), Some(tiles), Some(blobs), Some(counts)) = (
            ctx.inputs.array("sorted"),
            ctx.inputs.array("cell_ranges"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("blobs"),
            ctx.outputs.array("counts"),
        ) else {
            return;
        };
        if pool.elements() * std::mem::size_of::<CellRange>() as u64 > ranges.size {
            ctx.error(format!(
                "Shape Particle Blobs: a pool of {} tiles is larger than its cell_ranges, which hold one range per pool element",
                pool.pool_tiles
            ));
            return;
        }
        let count = (blobs.size / std::mem::size_of::<FluidBlob>() as u64)
            .min(sorted.size / std::mem::size_of::<FluidParticle>() as u64)
            .min(counts.size / 4) as u32;
        if count == 0 {
            return;
        }
        let uniforms = Uniforms {
            cell_size: pool.cell_size,
            sim_cell_size,
            particle_scale,
            count,
            pool_tiles: pool.pool_tiles,
            _pad_h: 0,
            _pad0: 0,
            _pad1: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            self.shape_pipeline
                .as_ref()
                .expect("shape particle blobs pipeline prepared"),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: sorted,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: ranges,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: blobs,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: counts,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.shape_particle_blobs",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
    }
}

manifold_node_engine::param_tooltips!("node.shape_particle_blobs", {
    "particle_scale" => "How large each particle's blob is, as a multiple of the particle's radius. A particle with few neighbours draws smaller, down to its own radius.",
});

#[cfg(test)]
mod tests {
    #[test]
    fn composed_shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(&super::BODY)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&super::BODY)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(std::mem::size_of::<super::Uniforms>(), 32);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
