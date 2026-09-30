//! `node.particles_near_bins` — per bin of a particle sort, how many particles
//! sit within `reach` bins. Lets a lattice atom that searches the bins around
//! each node skip the nodes no particle reaches (86% of the Dam Break's level
//! set at 64). A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer};

use super::sort_particles_into_cells::{bin_param, int_param};
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::{CellRange, searched_bins};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NearUniforms {
    bins_x: i32,
    bins_y: i32,
    bins_z: i32,
    reach: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: ParticlesNearBins,
    type_id: "node.particles_near_bins",
    purpose: "For each bin of a particle sort's grid (bins_x/y/z, bins indexed i + bx·(j + by·k)), how many particles sit in the bins within `reach` bins of it: a (2·reach + 1)³ box clipped to the grid, reach 1 to 4. Slots past the grid hold 0.",
    inputs: {
        cell_ranges: Array(CellRange) required,
        bins_x: ScalarF32 optional, bins_y: ScalarF32 optional, bins_z: ScalarF32 optional,
    },
    outputs: {
        counts: Array(u32),
    },
    params: [
        bin_param!("bins_x", "Bins X"),
        bin_param!("bins_y", "Bins Y"),
        bin_param!("bins_z", "Bins Z"),
        int_param!("reach", "Reach (bins)", 1.0, 1.0, 4.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire cell_ranges and bins_x/y/z from node.sort_particles_into_cells. A zero count means no particle within reach, so a searching atom can skip that bin's neighbourhood: wire counts into node.particle_volume's near (reach 1 covers the bins it searches). counts is sized to the bin grid every frame, as the sort sizes its ranges. Nothing runs while the sort has no grid.",
    examples: [],
    picker: { label: "Particles Near Bins", category: Atom },
    summary: "Counts the particles around each cell of the particle grid, so later steps can skip the empty parts of the space.",
    category: Particles3D,
    role: Filter,
    aliases: ["occupancy", "neighbour count", "empty space skip", "near particles"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/particles_near_bins_body.wgsl"),
    input_access: [BufferGather],
    extra_fields: {
        counts: Option<GpuBuffer> = None,
    },
}

impl Primitive for ParticlesNearBins {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "counts"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "counts").then_some(self.counts.as_ref()).flatten()
    }

    fn array_output_capacity(&self, port: &str, _params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        // Provided storage: a one-count hint, sized to the bin grid at run time.
        (port == "counts").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let bins = ["bins_x", "bins_y", "bins_z"].map(|name| ctx.scalar_or_param(name, 0.0).round());
        let reach = ctx.param_f32("reach", 1.0).round().clamp(1.0, 4.0) as i32;
        let gpu = ctx.gpu_encoder();
        standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let Some(ranges) = ctx.inputs.array("cell_ranges") else {
            return;
        };
        if bins.iter().any(|&n| n < 1.0) {
            return;
        }
        let bins = match searched_bins(bins, ranges.size, "Particles Near Bins") {
            Ok(bins) => bins,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let total = bins.iter().map(|&n| u64::from(n)).product::<u64>();
        let bytes = total * 4;
        if self.counts.as_ref().is_none_or(|counts| counts.size < bytes) {
            let device = ctx.gpu_encoder().device;
            let created = crate::node_graph::scene_modifier_expand::admit_candidate_bytes(device.modifier_memory_snapshot(), bytes)
                .map_err(|error| error.to_string())
                .and_then(|()| device.try_create_buffer_shared(bytes));
            match created {
                Ok(buffer) => self.counts = Some(buffer),
                Err(error) => {
                    ctx.error(format!("Particles Near Bins: {total} bins need {bytes} bytes the device cannot give: {error}"));
                    return;
                }
            }
        }
        let counts = self.counts.as_ref().expect("counts allocated above");
        let pipeline = self.pipeline.as_ref().expect("pipeline created above");
        let uniforms = NearUniforms {
            bins_x: bins[0] as i32,
            bins_y: bins[1] as i32,
            bins_z: bins[2] as i32,
            reach,
            dispatch_count: total as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: ranges, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: counts, offset: 0 },
            ],
            [(total as u32).div_ceil(256), 1, 1],
            "node.particles_near_bins",
        );
    }
}
