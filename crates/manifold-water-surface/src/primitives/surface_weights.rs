//! `node.surface_weights` — per field-lattice node, how much smoothing the
//! water surface gets there and the water's velocity there. The weight is
//! 1 in bulk water and 0 on thin sheets and drops, read from how many
//! neighbours the nearby particles have; the constants live in
//! `manifold_water_liquid::field_build`.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_water_liquid::field_build::{
    FIELD_REDISTANCE_CELLS, NEIGHBOUR_COUNT_RADIUS_CELLS, WEIGHT_CARRY_RADIUS_CELLS,
    WEIGHT_FADE_END, WEIGHT_FADE_START, expected_bulk_neighbours,
};
use manifold_water_liquid::float_param;
use manifold_water_liquid::fluid_particles::CellRange;
use manifold_water_liquid::primitives::sort_particles_into_cells::{bin_param, read_searched_bins};

const SHADER: &str = include_str!("shaders/surface_weights.wgsl");
const THREADS: u32 = 64;

/// Field order identical to the shader's `Params`; every row is 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct WeightUniforms {
    lattice_min: [f32; 3],
    cell: f32,
    spacing: [f32; 3],
    bin_size: f32,
    nodes: [u32; 3],
    particle_capacity: u32,
    bins: [i32; 3],
    node_count: u32,
    count_reach: f32,
    carry_reach: f32,
    band: f32,
    fade_start: f32,
    fade_end: f32,
    inv_bin: f32,
    pad_a: f32,
    pad_b: f32,
}

/// The shader's constants from the node's inputs. `nodes` are the field
/// lattice's node counts as wired (whole numbers, at least 2 per axis); the
/// field cell is the smallest node spacing, and every reach is a multiple of
/// it from `field_build`.
fn weight_uniforms(
    center: [f32; 3],
    size: [f32; 3],
    nodes: [f32; 3],
    cell_size: f32,
    bins: [u32; 3],
    particle_capacity: u32,
    particles_per_cell: f32,
    sim_cell_size: f32,
) -> Result<WeightUniforms, String> {
    if nodes.iter().any(|n| !(n.is_finite() && n.round() >= 2.0)) {
        return Err(format!(
            "Surface Weights: a {}×{}×{} field lattice has fewer than 2 nodes on an axis. Wire nodes_x/y/z from the particle volume that made reference.",
            nodes[0], nodes[1], nodes[2]
        ));
    }
    let nodes = nodes.map(|n| n.round() as u32);
    if center.iter().any(|v| !v.is_finite()) || size.iter().any(|v| !(v.is_finite() && *v > 0.0)) {
        return Err("Surface Weights: the lattice box must be finite with a positive size".into());
    }
    if !(cell_size.is_finite() && cell_size > 0.0) {
        return Err("Surface Weights: cell_size must be finite and positive".into());
    }
    if !(particles_per_cell.is_finite() && particles_per_cell > 0.0) {
        return Err("Surface Weights: particles_per_cell must be finite and positive".into());
    }
    if !(sim_cell_size.is_finite() && sim_cell_size > 0.0) {
        return Err("Surface Weights: sim_cell_size must be finite and positive".into());
    }
    let node_count = nodes
        .iter()
        .try_fold(1u32, |total, &n| total.checked_mul(n))
        .filter(|&total| total.checked_mul(3).is_some())
        .ok_or_else(|| {
            format!(
                "Surface Weights: a {}×{}×{} field lattice has more nodes than a velocity index can address",
                nodes[0], nodes[1], nodes[2]
            )
        })?;
    let bins = bins.map(i32::try_from);
    let [Ok(bins_x), Ok(bins_y), Ok(bins_z)] = bins else {
        return Err("Surface Weights: a bin axis is larger than a search can index".into());
    };
    let spacing: [f32; 3] = std::array::from_fn(|a| size[a] / (nodes[a] - 1) as f32);
    let cell = spacing.into_iter().fold(f32::INFINITY, f32::min);
    // The density is per simulation cell; scale it to this field cell's volume.
    let per_field_cell = particles_per_cell * (cell / sim_cell_size).powi(3);
    let bulk = expected_bulk_neighbours(per_field_cell);
    Ok(WeightUniforms {
        lattice_min: std::array::from_fn(|a| center[a] - 0.5 * size[a]),
        cell,
        spacing,
        bin_size: cell_size,
        nodes,
        particle_capacity,
        bins: [bins_x, bins_y, bins_z],
        node_count,
        count_reach: NEIGHBOUR_COUNT_RADIUS_CELLS * cell,
        carry_reach: WEIGHT_CARRY_RADIUS_CELLS * cell,
        band: FIELD_REDISTANCE_CELLS * cell,
        fade_start: WEIGHT_FADE_START * bulk,
        fade_end: WEIGHT_FADE_END * bulk,
        inv_bin: 1.0 / cell_size,
        pad_a: 0.0,
        pad_b: 0.0,
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
                .try_create_buffer_shared(bytes)
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(slot.as_ref().expect("buffer was just allocated").clone())
}

manifold_node_engine::primitive! {
    name: SurfaceWeights,
    type_id: "node.surface_weights",
    purpose: "Per node of the water's field lattice, how much smoothing the surface gets and the water's velocity there. weights holds one f32 per node: 1 in bulk water, fading to 0 on thin sheets and drops, from how many neighbours the particles near the node have (each particle counts the live particles within 1.5 field cells, the nodes average those counts over the particles within 2 field cells with a poly6 kernel, and the mean fades from 18% to 36% of the bulk count that particles_per_cell implies for one field cell). velocity holds three f32 per node (x, y, z, node-major, node index x + nodes_x * (y + nodes_y * z)), the same kernel's mean particle velocity in metres per second. Both are zero at nodes outside the re-distanced band (|reference| at least 4 field cells) and where no particle is within reach. A pure function of this frame's particles: no history, no frame counter.",
    inputs: {
        particles: Array(FluidParticle) required,
        cell_ranges: Array(CellRange) required,
        reference: Array(f32) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        sim_cell_size: ScalarF32 optional,
        bins_x: ScalarF32 optional, bins_y: ScalarF32 optional, bins_z: ScalarF32 optional,
        particles_per_cell: ScalarF32 optional,
    },
    outputs: {
        weights: Array(f32),
        velocity: Array(f32),
    },
    params: [
        float_param!("center_x", "Center X", 0.0, -1000.0, 1000.0),
        float_param!("center_y", "Center Y", 0.0, -1000.0, 1000.0),
        float_param!("center_z", "Center Z", 0.0, -1000.0, 1000.0),
        float_param!("size_x", "Size X", 4.0, 0.001, 1000.0),
        float_param!("size_y", "Size Y", 4.0, 0.001, 1000.0),
        float_param!("size_z", "Size Z", 4.0, 0.001, 1000.0),
        float_param!("nodes_x", "Field Nodes X", 2.0, 2.0, 16384.0),
        float_param!("nodes_y", "Field Nodes Y", 2.0, 2.0, 16384.0),
        float_param!("nodes_z", "Field Nodes Z", 2.0, 2.0, 16384.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.001, 100.0),
        float_param!("sim_cell_size", "Simulation Cell Size", 0.0625, 0.001, 100.0),
        bin_param!("bins_x", "Bins X"),
        bin_param!("bins_y", "Bins Y"),
        bin_param!("bins_z", "Bins Z"),
        float_param!("particles_per_cell", "Particles Per Simulation Cell", 8.0, 0.001, 1000.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire particles and cell_ranges from one node.sort_particles_into_cells (sorted and cell_ranges, with its bins_x/y/z, cell_size and the same center/size box), reference from the re-distanced field and nodes_x/y/z from the node.particle_volume that made it (volume_nodes_x/y/z, so the lattice box is also the sort box). particles_per_cell is the simulation's particles per simulation cell (8 in the FLIP default) and sim_cell_size that cell's side in metres (wire it from the simulation's cell size); the node scales the density to the field cell, so the bulk count follows Surface Detail. cell_ranges must hold one range per bin and reference at least one value per node, or nothing runs, a named error. The field cell is the smallest node spacing of the box. Feed weights and velocity to the field smoothing and the surface march. Two compute passes in order (counts, then weights) with an owned output buffer, so it is a graph fusion boundary.",
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
        let nodes = inputs
            .iter()
            .find(|(name, _)| *name == "reference")
            .map(|&(_, n)| n)?;
        match port {
            "weights" => Some(nodes),
            "velocity" => Some(nodes.saturating_mul(3)),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        {
            let gpu = ctx.gpu_encoder();
            if self.counts_pipeline.is_none() {
                self.counts_pipeline = Some(gpu.device.create_compute_pipeline(
                    SHADER,
                    "cs_counts",
                    "node.surface_weights",
                ));
            }
            if self.weights_pipeline.is_none() {
                self.weights_pipeline = Some(gpu.device.create_compute_pipeline(
                    SHADER,
                    "cs_weights",
                    "node.surface_weights",
                ));
            }
        }
        let (Some(particles), Some(ranges), Some(reference)) = (
            ctx.inputs.array("particles"),
            ctx.inputs.array("cell_ranges"),
            ctx.inputs.array("reference"),
        ) else {
            ctx.error("Surface Weights: particles, cell_ranges and reference must all be wired");
            return;
        };
        let bins = match read_searched_bins(ctx, ranges.size, "Surface Weights") {
            Ok(Some(bins)) => bins,
            Ok(None) => {
                ctx.error("Surface Weights: the sort has no bin grid yet (bins_x/y/z are 0)");
                return;
            }
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let center =
            ["center_x", "center_y", "center_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let size = ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0));
        let capacity = u32::try_from(particles.size / std::mem::size_of::<FluidParticle>() as u64)
            .unwrap_or(u32::MAX);
        let uniforms = match weight_uniforms(
            center,
            size,
            nodes,
            ctx.scalar_or_param("cell_size", 0.0625),
            bins,
            capacity,
            ctx.scalar_or_param("particles_per_cell", 8.0),
            ctx.scalar_or_param("sim_cell_size", 0.0625),
        ) {
            Ok(uniforms) => uniforms,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let node_count = u64::from(uniforms.node_count);
        if reference.size / 4 < node_count {
            ctx.error(format!(
                "Surface Weights: reference holds {} values; a {}×{}×{} field lattice needs {node_count}",
                reference.size / 4,
                uniforms.nodes[0],
                uniforms.nodes[1],
                uniforms.nodes[2]
            ));
            return;
        }

        let buffers = {
            let gpu = ctx.gpu_encoder();
            let counts = ensure_buffer(&mut self.counts, gpu.device, u64::from(capacity) * 4);
            let weights = ensure_buffer(&mut self.weights, gpu.device, node_count * 4);
            let velocity = ensure_buffer(&mut self.velocity, gpu.device, node_count * 12);
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
            [uniforms.node_count.div_ceil(THREADS), 1, 1],
            "node.surface_weights.weights",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(SHADER).expect("surface weights shader parses");
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
        assert_eq!(std::mem::size_of::<WeightUniforms>(), 96);
    }

    #[test]
    fn uniforms_follow_the_smallest_node_spacing_of_a_non_cubic_lattice() {
        let u = weight_uniforms(
            [0.5, -1.0, 2.0],
            [2.0, 1.5, 2.5],
            [33.0, 25.0, 41.0],
            0.125,
            [16, 12, 20],
            1000,
            8.0,
            0.125,
        )
        .unwrap();
        let bulk = expected_bulk_neighbours(1.0);
        assert_eq!(u.cell, 0.0625);
        assert_eq!(u.count_reach, 0.09375);
        assert_eq!(u.carry_reach, 0.125);
        assert_eq!(u.band, 0.25);
        assert_eq!(u.fade_start, WEIGHT_FADE_START * bulk);
        assert_eq!(u.fade_end, WEIGHT_FADE_END * bulk);
        assert_eq!(u.lattice_min, [-0.5, -1.75, 0.75]);
        assert_eq!(u.nodes, [33, 25, 41]);
        assert_eq!(u.node_count, 33 * 25 * 41);
        assert_eq!(u.bins, [16, 12, 20]);
        assert_eq!(u.inv_bin, 8.0);
    }

    #[test]
    fn bulk_count_follows_the_field_cell() {
        // Field cell 0.0625. The simulation cell at the field cell's size keeps
        // 8 per cell; a simulation cell four times as wide holds 64 times as
        // many field-cell volumes, so 8 per cell is 8 / 64 per field cell.
        let at = |sim_cell: f32| {
            weight_uniforms(
                [0.5, -1.0, 2.0],
                [2.0, 1.5, 2.5],
                [33.0, 25.0, 41.0],
                0.125,
                [16, 12, 20],
                1000,
                8.0,
                sim_cell,
            )
            .unwrap()
        };
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
    fn a_lattice_with_one_node_on_an_axis_is_an_error() {
        let one = weight_uniforms(
            [0.0; 3],
            [2.0; 3],
            [33.0, 1.0, 41.0],
            0.125,
            [4; 3],
            10,
            8.0,
            0.125,
        );
        assert!(one.is_err());
        let empty_bins =
            weight_uniforms([0.0; 3], [2.0; 3], [33.0; 3], 0.0, [4; 3], 10, 8.0, 0.125);
        assert!(empty_bins.is_err());
    }

    #[test]
    fn a_non_positive_or_nan_sim_cell_is_an_error() {
        for sim_cell in [0.0, f32::NAN] {
            let bad = weight_uniforms(
                [0.0; 3], [2.0; 3], [33.0; 3], 0.125, [4; 3], 10, 8.0, sim_cell,
            );
            assert!(bad.is_err(), "sim_cell_size {sim_cell}");
        }
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "surface_weights_gpu_tests.rs"]
mod gpu_tests;
