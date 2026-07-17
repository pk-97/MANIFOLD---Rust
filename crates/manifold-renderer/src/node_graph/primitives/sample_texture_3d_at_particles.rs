//! `node.sample_volume_at_particles` — trilinear sample of a vec3
//! `Texture3D` at each particle's `position.xyz`, emit `Array<[f32; 3]>`.
//!
//! The 3D sibling of `node.sample_image_at_particles`. Each live
//! particle reads the volume field's RGB at its current position and
//! writes it into the per-particle force buffer (overwrite, not add —
//! this is the first contribution to the FluidSim3D force accumulation,
//! matching the legacy `force = textureSampleLevel(t_field, ...).xyz`).
//!
//! Decomposed out of the fused `node.fluid_simulate_3d`. Compose with
//! `node.turbulence_3d`,
//! `node.move_particles_3d`, and `node.keep_in_box_3d` for
//! the full 3D advection chain.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: the `active_count` param (Int → i32) then
/// the codegen-injected `dispatch_count`, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SampleUniforms {
    active_count: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: SampleTexture3DAtParticles,
    type_id: "node.sample_volume_at_particles",
    purpose: "Per-particle trilinear sample of a vec3 Texture3D at each particle's position.xyz. Output: Array<[f32; 3]> of the volume's RGB per particle (overwrite, not add — seeds the per-particle force buffer). The 3D sibling of node.sample_image_at_particles; the generic volumetric field-read atom for any 3D particle pipeline. Decomposed out of the fused node.fluid_simulate_3d.",
    inputs: {
        particles: Array(Particle) required,
        field: Texture3D required,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array([f32; 3]),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("active_count"),
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(100_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Output capacity follows the input `particles` array. Samples are trilinear via the default clamp-edge sampler (matches the legacy fluid_simulate_3d field read). Writes the RGB at position.xyz directly — the force buffer is seeded here and accumulated by downstream force atoms (simplex_noise_force_3d, diffuse_force_3d, container_repel_force_3d) before node.move_particles_3d integrates it. Output entries for indices >= active_count are uninitialised.",
    examples: ["FluidSim3D"],
    picker: { label: "Sample Volume for Particles (3D)", category: Atom },
    summary: "Reads a 3D volume at each particle's position, so particles can pick up a value from a density or flow field they pass through.",
    category: Particles3D,
    role: Filter,
    aliases: ["sample volume", "sample texture 3d at particles", "read 3d texture", "trilinear"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/sample_texture_3d_at_particles_body.wgsl"),
}

impl Primitive for SampleTexture3DAtParticles {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities
                .iter()
                .find(|(p, _)| *p == "particles")
                .map(|(_, n)| *n)
        } else {
            None
        }
    }

    // run() dispatches `active_count` threads, not pool capacity — a fused
    // region containing this atom caps its dispatch the same way.
    fn fused_dispatch_count_param(&self) -> Option<&'static str> {
        Some("active_count")
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = ctx.scalar_or_param("active_count", 100_000.0).round().max(0.0) as u32;

        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(field) = ctx.inputs.texture_3d("field") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (particles.size / particle_size) as u32;
        let active_count = active_count.min(capacity);
        if active_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident + Texture3D path). sample_texture_3d_at_particles.wgsl is
            // the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.sample_volume_at_particles standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.sample_volume_at_particles",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = SampleUniforms {
            active_count: active_count as i32,
            dispatch_count: active_count,
            _pad0: 0,
            _pad1: 0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: field,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: out,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.sample_volume_at_particles",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_particles_in_texture3d_in_and_array_vec3_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();
        let vec3_layout = ArrayType::of_known::<[f32; 3]>();

        assert_eq!(
            SampleTexture3DAtParticles::TYPE_ID,
            "node.sample_volume_at_particles"
        );
        let names: Vec<&str> = SampleTexture3DAtParticles::INPUTS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(names, vec!["particles", "field", "active_count"]);
        assert_eq!(
            SampleTexture3DAtParticles::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert!(SampleTexture3DAtParticles::INPUTS[0].required);
        assert_eq!(SampleTexture3DAtParticles::INPUTS[1].ty, PortType::Texture3D);
        assert!(SampleTexture3DAtParticles::INPUTS[1].required);
        assert!(!SampleTexture3DAtParticles::INPUTS[2].required);

        assert_eq!(SampleTexture3DAtParticles::OUTPUTS.len(), 1);
        assert_eq!(SampleTexture3DAtParticles::OUTPUTS[0].name, "out");
        assert_eq!(
            SampleTexture3DAtParticles::OUTPUTS[0].ty,
            PortType::Array(vec3_layout)
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SampleTexture3DAtParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(
            node.type_id().as_str(),
            "node.sample_volume_at_particles"
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain TEXTURE-COINCIDENT (3D) parity oracle (freeze §12). The
    //! generated kernel binds a Texture3D + sampler into the buffer kernel and
    //! the body trilinear-samples it at each particle's position.xyz; it must
    //! reproduce the hand kernel force-for-force. Both sample the same volume +
    //! sampler → identical regardless of content.
    use super::*;
    use crate::generators::compute_common::Particle;
    use half::f16;
    use manifold_gpu::{
        GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    fn gradient_volume(device: &manifold_gpu::GpuDevice, n: u32) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (n * n * n * 4) as usize];
        for z in 0..n {
            for y in 0..n {
                for x in 0..n {
                    let i = (((z * n + y) * n + x) * 4) as usize;
                    px[i] = f16::from_f32(x as f32 / (n - 1) as f32);
                    px[i + 1] = f16::from_f32(y as f32 / (n - 1) as f32);
                    px[i + 2] = f16::from_f32(z as f32 / (n - 1) as f32);
                    px[i + 3] = f16::from_f32(1.0);
                }
            }
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: n,
            height: n,
            depth: n,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D3,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "sample-3d-at-particles-test",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    fn dispatch_sample3d(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        particles: &[Particle],
        tex: &GpuTexture,
        uniform: &[u8],
        count: u32,
    ) -> Vec<[f32; 3]> {
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "sample3d-oracle");
        let p_buf = device.create_buffer_shared(std::mem::size_of_val(particles) as u64);
        let out_buf = device.create_buffer_shared(count as u64 * 12);
        unsafe {
            p_buf.write(0, bytemuck::cast_slice(particles));
        }
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let mut enc = device.create_encoder("sample3d-oracle");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform },
                GpuBinding::Buffer { binding: 1, buffer: &p_buf, offset: 0 },
                GpuBinding::Texture { binding: 2, texture: tex },
                GpuBinding::Sampler { binding: 3, sampler: &sampler },
                GpuBinding::Buffer { binding: 4, buffer: &out_buf, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "sample3d-oracle",
        );
        enc.commit_and_wait_completed();
        let ptr = out_buf.mapped_ptr().expect("shared out buffer");
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const [f32; 3], count as usize) };
        slice.to_vec()
    }

    #[test]
    fn generated_sample3d_matches_hand_kernel() {
        let device = crate::test_device();
        let vol = gradient_volume(&device, 8);
        let mk = |pos: [f32; 3]| Particle {
            position: pos,
            _pad0: 0.0,
            velocity: [0.0; 3],
            life: 1.0,
            age: 0.0,
            _pad1: [0.0; 3],
            color: [0.0; 4],
        };
        let particles = [
            mk([0.1, 0.2, 0.3]),
            mk([0.5, 0.6, 0.7]),
            mk([0.9, 0.3, 0.1]),
            mk([0.33, 0.77, 0.5]),
        ];
        let n = particles.len() as u32;

        let mut hand = Vec::new();
        hand.extend_from_slice(&n.to_le_bytes());
        hand.extend_from_slice(&[0u8; 12]);

        let mut gen_bytes = Vec::new();
        gen_bytes.extend_from_slice(&(n as i32).to_le_bytes());
        gen_bytes.extend_from_slice(&n.to_le_bytes());
        gen_bytes.extend_from_slice(&[0u8; 8]);

        let hand_wgsl = include_str!("shaders/sample_texture_3d_at_particles.wgsl");
        let gen_wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<SampleTexture3DAtParticles>()
                .expect("sample_texture_3d_at_particles buffer codegen");
        assert!(gen_wgsl.contains("var tex_field: texture_3d<f32>"), "Texture3D bound into the kernel");

        let from_hand = dispatch_sample3d(&device, hand_wgsl, &particles, &vol, &hand, n);
        let from_gen = dispatch_sample3d(&device, &gen_wgsl, &particles, &vol, &gen_bytes, n);

        for i in 0..n as usize {
            for c in 0..3 {
                assert!(
                    (from_hand[i][c] - from_gen[i][c]).abs() < 1e-5,
                    "particle {i} force[{c}]: hand={} gen={}",
                    from_hand[i][c],
                    from_gen[i][c]
                );
            }
        }
    }
}
