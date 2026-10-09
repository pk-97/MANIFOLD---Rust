//! Uses the search-radius ratio (1.5 radii) from FLIP Fluids particlemesher.cpp `_searchRadiusFactor` (MIT); see THIRD_PARTY_NOTICES.md.
//! `node.shape_particle_blobs` — one anisotropic surface kernel per sorted
//! liquid particle (Yu & Turk 2010; GPU_FLUID_SURFACE_DESIGN.md D14). A
//! per-element gather over the sorted particles' bins, on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::float_param;
use super::sort_particles_into_cells::{bin_param, read_searched_bins};
use crate::primitives::standalone_pipeline::standalone_pipeline;
use crate::exec::effect_node::{EffectNodeContext, ParamValues};
use crate::particles::{FluidParticle};
use crate::water::fluid_particles::{CellRange, FluidBlob};
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::primitive::Primitive;

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
    bins_x: i32,
    bins_y: i32,
    bins_z: i32,
    dispatch_count: u32,
}

crate::primitive! {
    name: ShapeParticleBlobs,
    type_id: "node.shape_particle_blobs",
    purpose: "Give each sorted liquid particle an ellipsoidal surface kernel (Yu & Turk 2010). Neighbours within particle_scale × radius set a weighted-mean centre (smoothing = 0 keeps the particle, 1 uses the mean) and a covariance; its principal axes set a volume-preserving ellipsoid whose axis ratio is capped at stretch. Fewer than min_neighbours gives a sphere. With no neighbour within two physical radii the kernel shrinks toward isolated_scale by three.",
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
        bins_x: ScalarF32 optional, bins_y: ScalarF32 optional, bins_z: ScalarF32 optional,
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
        bin_param!("bins_x", "Bins X"),
        bin_param!("bins_y", "Bins Y"),
        bin_param!("bins_z", "Bins Z"),
    ],
    depth_rule: Terminal,
    composition_notes: "Feed it node.sort_particles_into_cells' outputs with the same box and cell_size, and wire the sort's bins_x/y/z: the bins are the sort's, never worked out again on the GPU, and cell_ranges must hold one range per bin or nothing runs (a named error). All three unwired (a graph from before these wires) takes the sort's CPU rule on the shared box, checked the same way. Kernel size and neighbour searches are uncapped by bin width. shape_off.w carries the centre displacement for node.blob_bounds; wire its bounds to particle_volume and lattice_bricks to accelerate their exact support search. Stretch 1 writes exact spheres and skips the covariance pass, about half the kernel's cost. Live params: changing any of them reshapes the next frame's surface without touching the simulation. Output slots of inactive particles have radius 0.",
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
            smoothing: ctx.scalar_or_param("smoothing", 0.9),
            isolated_scale: ctx.scalar_or_param("isolated_scale", 1.0).clamp(0.25, 1.0),
            min_neighbours: ctx.scalar_or_param("min_neighbours", 8.0).round() as i32,
            bins_x: 0,
            bins_y: 0,
            bins_z: 0,
            dispatch_count: 0,
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
        let bins = match read_searched_bins(ctx, ranges.size, "Shape Particle Blobs") {
            Ok(Some(bins)) => bins,
            Ok(None) => return,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let count = (blobs.size / std::mem::size_of::<FluidBlob>() as u64)
            .min(sorted.size / std::mem::size_of::<FluidParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        let [bins_x, bins_y, bins_z] = bins.map(|n| n as i32);
        let uniforms = BlobUniforms { bins_x, bins_y, bins_z, dispatch_count: count, ..uniforms };
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

crate::param_tooltips!("node.shape_particle_blobs", {
    "stretch" => "Stretch nearby particles along the flow to shape sheets and splashes.",
    "smoothing" => "Move particle centres toward their neighbours for a calmer surface.",
});

#[cfg(test)]
mod tests {
    #[test]
    fn fluid_shape_fixed_sphere_shader_validates() {
        let source = crate::freeze::codegen::standalone_for_spec::<
            super::ShapeParticleBlobs,
        >().expect("shape codegen");
        let module = naga::front::wgsl::parse_str(&source).expect("shape WGSL");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all(),
        ).validate(&module).expect("shape validation");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::freeze::codegen;
    use crate::testkit::liquid_surface::{
        Harness, Lattice, read, sort_and_shape,
    };
    use bytemuck::Zeroable;

    #[test]
    fn fluid_shape_fixed_spheres_match_neighbour_gather() {
        let mut harness = Harness::new();
        let lattice = Lattice {
            center: [0.0; 3],
            size: [4.0; 3],
            cell: 0.25,
        };
        let mut particles = Vec::new();
        // A populated cloud, isolated droplets, a boundary particle and inactive
        // capacity exercise the same sorted inputs in both shader variants.
        for i in 0..257 {
            let position = if i < 250 {
                [
                    (i % 10) as f32 * 0.013,
                    ((i / 10) % 5) as f32 * 0.017,
                    (i / 50) as f32 * 0.019,
                ]
            } else {
                [-1.9 + (i - 250) as f32 * 0.55, -1.9, 1.9]
            };
            particles.push(FluidParticle {
                position_radius: [
                    position[0],
                    position[1],
                    position[2],
                    0.017 + (i % 7) as f32 * 0.009,
                ],
                velocity: [0.0; 3],
                id: i + 1,
            });
        }
        particles.extend_from_slice(&[FluidParticle::zeroed(); 3]);
        let source = codegen::standalone_for_spec::<ShapeParticleBlobs>().unwrap();
        let condition = "if smoothing == 0.0 && stretch <= 1.0 && isolated_scale == 1.0";
        assert_eq!(
            source.matches(condition).count(),
            1,
            "reference must disable only the shortcut"
        );
        let reference_source = source.replacen(condition, "if false", 1);
        let reference_pipeline = harness.device.create_compute_pipeline(
            &reference_source,
            codegen::ENTRY,
            "fixed sphere neighbour reference",
        );
        let mut reference = ShapeParticleBlobs::new();
        reference.pipeline = Some(reference_pipeline);
        for scale in [0.25, 2.2, 3.0, 8.0] {
            let shape = [
                ("particle_scale", scale),
                ("stretch", 1.0),
                ("smoothing", 0.0),
                ("isolated_scale", 1.0),
                ("min_neighbours", 8.0),
            ];
            let (_, _, actual, (sorted, ranges, _)) =
                sort_and_shape(&mut harness, &lattice, &particles, 257, &shape);
            let (out, buffer) = harness.array::<FluidBlob>(&[], particles.len());
            let (_, errors) = harness.run(
                &mut reference,
                &[("sorted", sorted), ("cell_ranges", ranges)],
                &[("blobs", out)],
                &lattice.params(&shape),
            );
            assert!(errors.is_empty(), "{errors:?}");
            let expected = read::<FluidBlob>(&buffer, particles.len());
            for (word, (&actual, &expected)) in bytemuck::cast_slice::<_, u32>(&actual)
                .iter().zip(bytemuck::cast_slice::<_, u32>(&expected)).enumerate()
            {
                assert_eq!(actual, expected, "scale {scale}, blob word {word}");
            }
        }
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
