//! `node.sample_image_at_particles` — bilinear texture sample at
//! each particle's UV position, emit `Array<vec2<f32>>`.
//!
//! The per-particle texture-read atom. Pair with any pre-computed
//! velocity field / density field / per-particle colour LUT — the
//! atom doesn't know what kind of field it's reading, only that each
//! live particle wants the field's value at its current UV.
//!
//! Splits the legacy fused `integrate_particles` kernel into its
//! field-sample step. Compose with `node.move_particles` and
//! `node.wrap_around` for the full advection chain.

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
    name: SampleTextureAtParticles,
    type_id: "node.sample_image_at_particles",
    purpose: "Per-particle bilinear sample of a Texture2D at each particle's position.xy. Output: Array<vec2<f32>> of the texture's RG channels per particle. The generic field-read atom for any particle pipeline — velocity fields, density samples, per-particle colour LUTs. Decomposed out of the legacy fused `integrate_particles` kernel.",
    inputs: {
        particles: Array(Particle) required,
        in: Texture2D required,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array([f32; 2]),
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
    composition_notes: "Output capacity follows the input `particles` array. Samples are bilinear via the default clamp-edge sampler (matches `integrate_particles`'s legacy behaviour). For toroidal flow fields, the input texture is sampled at p.position.xy directly — wrap if needed is the consumer's responsibility (typically a downstream `wrap_particles_torus`). Output is RG only; consumers that need a single channel can pair with `node.split_xy`. Output entries for indices ≥ active_count are uninitialised — downstream consumers must respect active_count too.",
    examples: [],
    picker: { label: "Sample Image for Particles", category: Atom },
    summary: "Reads the image colour underneath each particle, so the particles can pick up the look of whatever they fly over.",
    category: Particles2D,
    role: Filter,
    aliases: ["sample image", "sample texture at particles", "read texture", "pick color"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/sample_texture_at_particles_body.wgsl"),
}

impl Primitive for SampleTextureAtParticles {
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
        let Some(field) = ctx.inputs.texture_2d("in") else {
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
            // coincident + texture path — the body samples tex_in at the particle
            // position). sample_texture_at_particles.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.sample_image_at_particles standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.sample_image_at_particles",
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
            "node.sample_image_at_particles",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_particles_in_texture_in_and_array_vec2_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();
        let vec2_layout = ArrayType::of_known::<[f32; 2]>();

        assert_eq!(SampleTextureAtParticles::TYPE_ID, "node.sample_image_at_particles");
        let names: Vec<&str> = SampleTextureAtParticles::INPUTS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(names, vec!["particles", "in", "active_count"]);
        assert_eq!(
            SampleTextureAtParticles::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert!(SampleTextureAtParticles::INPUTS[0].required);
        assert_eq!(SampleTextureAtParticles::INPUTS[1].ty, PortType::Texture2D);
        assert!(SampleTextureAtParticles::INPUTS[1].required);
        assert!(!SampleTextureAtParticles::INPUTS[2].required);

        assert_eq!(SampleTextureAtParticles::OUTPUTS.len(), 1);
        assert_eq!(SampleTextureAtParticles::OUTPUTS[0].name, "out");
        assert_eq!(
            SampleTextureAtParticles::OUTPUTS[0].ty,
            PortType::Array(vec2_layout)
        );
    }

    #[test]
    fn active_count_port_shadows_param() {
        let has_port = SampleTextureAtParticles::INPUTS
            .iter()
            .any(|p| p.name == "active_count");
        let has_param = SampleTextureAtParticles::PARAMS
            .iter()
            .any(|p| p.name == "active_count");
        assert!(has_port);
        assert!(has_param);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SampleTextureAtParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.sample_image_at_particles");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain TEXTURE-COINCIDENT parity oracle (freeze §12) — the first
    //! atom exercising a texture bound INTO a buffer kernel. The generated kernel
    //! binds uniform(0)/particles(1)/tex(2)/samp(3)/out(4) — the body samples
    //! tex_in at each particle's position — and must reproduce the hand kernel
    //! force-for-force. Both sample the same texture + sampler at the same UV, so
    //! the result is identical regardless of the texture content.
    use super::*;
    use crate::generators::compute_common::Particle;
    use half::f16;
    use manifold_gpu::{
        GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    fn gradient_tex(device: &manifold_gpu::GpuDevice, w: u32, h: u32) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i] = f16::from_f32(x as f32 / (w - 1) as f32);
                px[i + 1] = f16::from_f32(y as f32 / (h - 1) as f32);
                px[i + 2] = f16::from_f32(0.5);
                px[i + 3] = f16::from_f32(1.0);
            }
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "sample-at-particles-test",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    fn dispatch_sample(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        particles: &[Particle],
        tex: &GpuTexture,
        uniform: &[u8],
        count: u32,
    ) -> Vec<[f32; 2]> {
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "sample-oracle");
        let p_buf = device.create_buffer_shared(std::mem::size_of_val(particles) as u64);
        let out_buf = device.create_buffer_shared(count as u64 * 8);
        unsafe {
            p_buf.write(0, bytemuck::cast_slice(particles));
        }
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let mut enc = device.create_encoder("sample-oracle");
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
            "sample-oracle",
        );
        enc.commit_and_wait_completed();
        let ptr = out_buf.mapped_ptr().expect("shared out buffer");
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const [f32; 2], count as usize) };
        slice.to_vec()
    }

    #[test]
    fn generated_sample_matches_hand_kernel() {
        let device = crate::test_device();
        let tex = gradient_tex(&device, 16, 16);
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
            mk([0.1, 0.2, 0.0]),
            mk([0.5, 0.6, 0.0]),
            mk([0.9, 0.3, 0.0]),
            mk([0.33, 0.77, 0.0]),
        ];
        let n = particles.len() as u32;

        // Hand layout: active_count(u32) + 3 pad words.
        let mut hand = Vec::new();
        hand.extend_from_slice(&n.to_le_bytes());
        hand.extend_from_slice(&[0u8; 12]);

        // Generated layout: active_count(i32), dispatch_count(u32), 2 pad words.
        let mut gen_bytes = Vec::new();
        gen_bytes.extend_from_slice(&(n as i32).to_le_bytes());
        gen_bytes.extend_from_slice(&n.to_le_bytes());
        gen_bytes.extend_from_slice(&[0u8; 8]);

        let hand_wgsl = include_str!("shaders/sample_texture_at_particles.wgsl");
        let gen_wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<SampleTextureAtParticles>()
                .expect("sample_texture_at_particles buffer codegen");
        assert!(gen_wgsl.contains("var tex_in: texture_2d<f32>"), "texture bound into the buffer kernel");
        assert!(gen_wgsl.contains("var samp: sampler"), "shared sampler bound");

        let from_hand = dispatch_sample(&device, hand_wgsl, &particles, &tex, &hand, n);
        let from_gen = dispatch_sample(&device, &gen_wgsl, &particles, &tex, &gen_bytes, n);

        for i in 0..n as usize {
            assert!(
                (from_hand[i][0] - from_gen[i][0]).abs() < 1e-5
                    && (from_hand[i][1] - from_gen[i][1]).abs() < 1e-5,
                "particle {i}: hand={:?} gen={:?}",
                from_hand[i],
                from_gen[i]
            );
        }
    }
}
