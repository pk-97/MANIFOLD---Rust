//! `node.shape_particle_blobs` — one anisotropic surface kernel per sorted
//! liquid particle (Yu & Turk 2010; GPU_FLUID_SURFACE_DESIGN.md D14). A
//! per-element gather over the sorted particles' bins, on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::{CellRange, FluidBlob, FluidParticle};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlobUniforms {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    cell_size: f32,
    particle_scale: f32,
    stretch: f32,
    smoothing: f32,
    isolated_scale: f32,
    min_neighbours: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: ShapeParticleBlobs,
    type_id: "node.shape_particle_blobs",
    purpose: "Give each sorted liquid particle an ellipsoidal surface kernel (Yu & Turk 2010). Neighbours within particle_scale × radius (at most one bin, cell_size) set a weighted-mean centre (smoothing = 0 keeps the particle, 1 uses the mean) and a covariance; its principal axes set a volume-preserving ellipsoid whose axis ratio is capped at stretch. Fewer than min_neighbours gives a sphere. With no neighbour within two physical radii the kernel shrinks toward isolated_scale by three.",
    inputs: {
        sorted: Array(FluidParticle) required,
        cell_ranges: Array(CellRange) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        particle_scale: ScalarF32 optional,
        stretch: ScalarF32 optional,
        smoothing: ScalarF32 optional,
        isolated_scale: ScalarF32 optional,
        min_neighbours: ScalarF32 optional,
    },
    outputs: {
        blobs: Array(FluidBlob),
    },
    params: [
        float_param!("center_x", "Center X", 0.0, -1000.0, 1000.0),
        float_param!("center_y", "Center Y", 0.0, -1000.0, 1000.0),
        float_param!("center_z", "Center Z", 0.0, -1000.0, 1000.0),
        float_param!("size_x", "Size X", 4.0, 0.001, 1000.0),
        float_param!("size_y", "Size Y", 4.0, 0.001, 1000.0),
        float_param!("size_z", "Size Z", 4.0, 0.001, 1000.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.001, 100.0),
        float_param!("particle_scale", "Particle Scale", 3.0, 0.25, 8.0),
        float_param!("stretch", "Stretch", 4.0, 1.0, 16.0),
        float_param!("smoothing", "Centre Smoothing", 0.9, 0.0, 1.0),
        float_param!("isolated_scale", "Isolated Droplet Scale", 1.0, 0.25, 1.0),
        ParamDef {
            name: Cow::Borrowed("min_neighbours"),
            label: "Min Neighbours",
            ty: ParamType::Int,
            default: ParamValue::Float(8.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Feed it node.sort_particles_into_cells' outputs with the same box and cell_size. The kernel never reaches past 0.9 bin from its particle (the band node.particle_volume's distance cap relies on), so cell_size bounds both the look and the cost: particle_scale above 0.9 × cell_size / radius has no further effect. Live params: changing any of them reshapes the next frame's surface without touching the simulation. Output slots of inactive particles have radius 0.",
    examples: [],
    picker: { label: "Shape Particle Blobs", category: Atom },
    summary: "Stretches each liquid particle along the shape of its neighbours, so thin sheets and streams stay thin instead of turning into beads.",
    category: Particles3D,
    role: Map,
    aliases: ["anisotropic kernels", "yu turk", "particle ellipsoids", "liquid blobs"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/shape_particle_blobs_body.wgsl"),
    input_access: [BufferGather, BufferGather],
}

impl Primitive for ShapeParticleBlobs {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "blobs")
            .then(|| inputs.iter().find(|(name, _)| *name == "sorted").map(|&(_, n)| n))
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let [center_x, center_y, center_z] =
            ["center_x", "center_y", "center_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let [size_x, size_y, size_z] = ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let uniforms = BlobUniforms {
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            cell_size: ctx.scalar_or_param("cell_size", 0.0625),
            particle_scale: ctx.scalar_or_param("particle_scale", 3.0),
            stretch: ctx.scalar_or_param("stretch", 4.0),
            smoothing: ctx.scalar_or_param("smoothing", 0.9).clamp(0.0, 1.0),
            isolated_scale: ctx.scalar_or_param("isolated_scale", 1.0).clamp(0.25, 1.0),
            min_neighbours: ctx.scalar_or_param("min_neighbours", 8.0).round() as i32,
            dispatch_count: 0,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(sorted), Some(ranges), Some(blobs)) = (
            ctx.inputs.array("sorted"),
            ctx.inputs.array("cell_ranges"),
            ctx.outputs.array("blobs"),
        ) else {
            return;
        };
        let count = (blobs.size / std::mem::size_of::<FluidBlob>() as u64)
            .min(sorted.size / std::mem::size_of::<FluidParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        let uniforms = BlobUniforms { dispatch_count: count, ..uniforms };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: sorted, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: ranges, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: blobs, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.shape_particle_blobs",
        );
    }
}
