//! `node.shape_particle_blobs` — one spherical surface kernel per sorted
//! liquid particle: its centre and its drawn radius (Particle Scale × the
//! particle's radius). A per-element map on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_water_liquid::float_param;
use manifold_water_liquid::fluid_particles::FluidBlob;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlobUniforms {
    particle_scale: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

manifold_node_engine::primitive! {
    name: ShapeParticleBlobs,
    type_id: "node.shape_particle_blobs",
    purpose: "Give each sorted liquid particle a spherical surface kernel: the particle's centre and a radius of particle_scale × the particle's own radius. The level set is the distance to the nearest sphere surface. An inactive particle (radius 0) gives a kernel of radius 0.",
    inputs: {
        sorted: Array(FluidParticle) required,
        particle_scale: ScalarF32 optional,
    },
    outputs: {
        blobs: Array(FluidBlob),
    },
    params: [
        float_param!("particle_scale", "Particle Scale", 3.0, 0.25, 8.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Feed it the sorted particles from node.sort_particles_into_cells. Wire its blobs to node.blob_bounds, node.particle_volume and node.lattice_bricks. Particle Scale is live: changing it reshapes the next frame's surface without touching the simulation. Output slots of inactive particles have radius 0.",
    examples: [],
    picker: { label: "Shape Particle Blobs", category: Atom },
    summary: "Turns each liquid particle into a round blob sized by its radius, which the liquid surface is built from.",
    category: Particles3D,
    role: Map,
    aliases: ["particle spheres", "liquid blobs", "particle kernels"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/shape_particle_blobs_body.wgsl"),
    input_access: [Coincident],
}

impl Primitive for ShapeParticleBlobs {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "blobs")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "sorted")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let particle_scale = ctx.scalar_or_param("particle_scale", 3.0);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(sorted), Some(blobs)) = (ctx.inputs.array("sorted"), ctx.outputs.array("blobs"))
        else {
            return;
        };
        let count = (blobs.size / std::mem::size_of::<FluidBlob>() as u64)
            .min(sorted.size / std::mem::size_of::<FluidParticle>() as u64)
            as u32;
        if count == 0 {
            return;
        }
        let uniforms = BlobUniforms {
            particle_scale,
            dispatch_count: count,
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
                    buffer: sorted,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: blobs,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.shape_particle_blobs",
        );
    }
}

manifold_node_engine::param_tooltips!("node.shape_particle_blobs", {
    "particle_scale" => "How large each particle's blob is, as a multiple of the particle's radius.",
});

#[cfg(test)]
mod tests {
    #[test]
    fn fluid_shape_fixed_sphere_shader_validates() {
        let source = manifold_node_engine::freeze::codegen::standalone_for_spec::<
            super::ShapeParticleBlobs,
        >()
        .expect("shape codegen");
        let module = naga::front::wgsl::parse_str(&source).expect("shape WGSL");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("shape validation");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::testkit::liquid_surface::{Lattice, sort_and_shape};
    use bytemuck::Zeroable;
    use manifold_node_engine::testkit::array_harness::Harness;

    #[test]
    fn blobs_are_particle_centres_with_scaled_radii() {
        let mut harness = Harness::new();
        let lattice = Lattice {
            center: [0.0; 3],
            size: [4.0; 3],
            cell: 0.25,
        };
        let mut particles = Vec::new();
        // Mixed radii, every seventh slot inactive, plus zeroed capacity.
        for i in 0..300u32 {
            let radius = if i % 7 == 3 {
                0.0
            } else {
                0.017 + (i % 11) as f32 * 0.009
            };
            particles.push(FluidParticle {
                position_radius: [
                    -1.9 + (i % 13) as f32 * 0.29,
                    -1.9 + ((i / 13) % 9) as f32 * 0.41,
                    -1.9 + (i / 117) as f32 * 1.3,
                    radius,
                ],
                velocity: [0.0; 3],
                id: i + 1,
            });
        }
        particles.extend_from_slice(&[FluidParticle::zeroed(); 3]);
        for scale in [0.25, 2.2, 3.0, 8.0] {
            let shape = [("particle_scale", scale)];
            let (sorted, _, blobs, _) =
                sort_and_shape(&mut harness, &lattice, &particles, 300, &shape);
            assert_eq!(blobs.len(), sorted.len());
            let mut active = 0;
            for (blob, particle) in blobs.iter().zip(&sorted) {
                let [x, y, z, r] = particle.position_radius;
                let expected = if r > 0.0 {
                    active += 1;
                    [x, y, z, scale * r]
                } else {
                    [0.0; 4]
                };
                assert_eq!(blob.center_radius, expected, "scale {scale}");
            }
            assert!(active > 200 && active < sorted.len());
        }
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
