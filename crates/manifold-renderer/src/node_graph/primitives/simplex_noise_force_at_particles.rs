//! `node.turbulence` — per-particle 2D simplex
//! noise force added in-place to an Array<vec2<f32>> force buffer.
//!
//! For each live particle i: evaluate 2D simplex noise at the
//! particle's position (X and Y channels offset by 100 in noise UV
//! space for decorrelation), scale by `amplitude`, optionally modulate
//! by a scalar texture sampled at the same UV (capped `m/(1+m)` × gain
//! boost — matches the legacy FluidSim density-adaptive noise formula),
//! and add to `forces[i]`. Aliased `in`/`out` Array<vec2>: a single
//! physical buffer, mutated in place.
//!
//! Why per-particle and not per-pixel? Same math, different work
//! domain. A texture-domain noise field evaluates simplex_noise_2d
//! at canvas-pixel UVs (~8.3M positions at 4K), then particles bilinear-
//! sample it. The atom evaluates at particle positions directly
//! (~2M at FluidSim defaults), then adds to the per-particle force
//! buffer. The output is mathematically equivalent; the cost is
//! independent of canvas resolution. Replaces a ~9-node texture
//! noise advection chain (simplex_field_2d × 2, pack_channels × 2,
//! scale_offset × 3, mix × 2) for any per-particle noise consumer.
//!
//! Optional `amplitude_modulator` Texture2D: when wired, samples the
//! modulator's R channel at the particle UV and applies
//! `amplitude * (1 + capped(m) * modulator_gain)`. When unwired,
//! amplitude is uniform. Both modes are bit-identical to the legacy
//! formula for the corresponding wire shape.
//!
//! Reusable for any per-particle force-augmentation pipeline that
//! wants spatially-coherent jitter without paying canvas-area cost.

use std::borrow::Cow;
use manifold_gpu::{
    GpuBinding, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`amplitude`,
/// `modulator_gain`, `z`, `noise_scale` f32, `active_count` Int → i32), then the
/// injected optional-texture flag `use_amplitude_modulator` (u32), then the
/// codegen-injected `dispatch_count`, padded to a 16-byte multiple. 7 words + 1
/// pad = 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NoiseUniforms {
    amplitude: f32,
    modulator_gain: f32,
    z: f32,
    noise_scale: f32,
    active_count: i32,
    use_amplitude_modulator: u32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: SimplexNoiseForceAtParticles,
    type_id: "node.turbulence",
    purpose: "Per-particle 2D simplex noise force added in-place to an Array<vec2<f32>> force buffer. Evaluates simplex_noise_2d at each particle's position (X/Y noise channels offset by 100 for decorrelation), scales by `amplitude`, optionally boosts by a scalar Texture2D sampled at the same UV (`amplitude * (1 + capped(m) * gain)`, capped = m/(1+m)), and adds to `forces[i]`. Aliased Array<vec2> in/out — one physical buffer, in-place mutation. Resolution-independent: work scales with particle count, not canvas area. Replaces a per-pixel texture noise chain (simplex_field × 2 + math + mix) for any per-particle noise consumer.",
    inputs: {
        in: Array([f32; 2]) required,
        particles: Array(Particle) required,
        amplitude_modulator: Texture2D optional,
        amplitude: ScalarF32 optional,
        modulator_gain: ScalarF32 optional,
        z: ScalarF32 optional,
        noise_scale: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array([f32; 2]),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amplitude"),
            label: "Amplitude",
            ty: ParamType::Float,
            default: ParamValue::Float(0.001),
            range: Some((0.0, 0.05)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("modulator_gain"),
            label: "Modulator Gain",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.0, 20.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("z"),
            label: "Z (Time)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("noise_scale"),
            label: "Noise Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
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
    composition_notes: "Aliased Array<vec2> in/out (one buffer, in-place add). `amplitude` is port-shadow so a control wire (LFO / clip-trigger envelope / outer-card slider) drives noise energy live; `z` is port-shadow so a `time × scalar` math chain animates the noise field through Z. Wire any scalar Texture2D into `amplitude_modulator` to localize the noise (canonical FluidSim use wires the density texture); leave unwired for spatially-coherent noise at uniform amplitude everywhere. Replaces ~9 canvas-sized nodes in FluidSim2D's per-pixel noise advection.",
    examples: ["FluidSim2D"],
    picker: { label: "Turbulence (simplex)", category: Atom },
    summary: "Pushes particles around with a flowing noise field, giving organic, swirling motion. The classic turbulence force.",
    category: Particles2D,
    role: Filter,
    aliases: ["turbulence", "simplex noise force at particles", "noise force", "flow", "simplex"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/simplex_noise_force_at_particles_body.wgsl"),
    extra_fields: {
        dummy_modulator: Option<GpuTexture> = None,
    },
}

impl Primitive for SimplexNoiseForceAtParticles {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities
                .iter()
                .find(|(p, _)| *p == "in")
                .map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    // run() dispatches `active_count` threads, not pool capacity — a fused
    // region containing this atom caps its dispatch the same way.
    fn fused_dispatch_count_param(&self) -> Option<&'static str> {
        Some("active_count")
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amplitude = ctx.scalar_or_param("amplitude", 0.001);
        let modulator_gain = ctx.scalar_or_param("modulator_gain", 2.0);
        let z = ctx.scalar_or_param("z", 0.0);
        let noise_scale = ctx.scalar_or_param("noise_scale", 2.0);
        let active_count = ctx
            .scalar_or_param("active_count", 100_000.0)
            .round()
            .max(0.0) as u32;

        let Some(in_forces) = ctx.inputs.array("in") else {
            return;
        };
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let modulator_wire = ctx.inputs.texture_2d("amplitude_modulator");
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let _ = out;

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let particle_capacity = (particles.size / particle_size) as u32;
        let force_capacity = (in_forces.size / 8) as u32;
        let active_count = active_count.min(particle_capacity).min(force_capacity);
        if active_count == 0 {
            return;
        }
        let has_modulator: u32 = if modulator_wire.is_some() { 1 } else { 0 };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // COINCIDENT in/particles + OPTIONAL Texture2D modulator + use-flag).
            // simplex_noise_force_at_particles.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.turbulence standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.turbulence",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        // Metal requires every declared shader binding to be present
        // at dispatch even when the kernel's `if has_modulator` branch
        // skips the sample. Cache a 1×1 white texture as the unwired
        // fallback bind; allocated once per instance.
        let dummy = self.dummy_modulator.get_or_insert_with(|| {
            let tex = gpu.device.create_texture(&GpuTextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                format: GpuTextureFormat::Rgba8Unorm,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::CPU_UPLOAD,
                label: "node.turbulence dummy modulator",
                mip_levels: 1,
            });
            gpu.device.upload_texture(&tex, &[255u8, 255, 255, 255]);
            tex
        });
        let modulator_tex = modulator_wire.unwrap_or(dummy);

        let uniforms = NoiseUniforms {
            amplitude,
            modulator_gain,
            z,
            noise_scale,
            active_count: active_count as i32,
            use_amplitude_modulator: has_modulator,
            dispatch_count: active_count,
            _pad0: 0,
        };

        // Generated binding order follows INPUTS: uniform(0), buf_in(1, force
        // read), buf_particles(2, read), tex_amplitude_modulator(3), samp(4),
        // buf_out(5, force read_write). `in`/`out` alias one force buffer →
        // bind in_forces to both 1 and 5.
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: in_forces,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: modulator_tex,
                },
                GpuBinding::Sampler {
                    binding: 4,
                    sampler,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: in_forces,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.turbulence",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_aliased_vec2_in_out_required_particles_and_optional_modulator() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();
        let vec2_layout = ArrayType::of_known::<[f32; 2]>();

        assert_eq!(
            SimplexNoiseForceAtParticles::TYPE_ID,
            "node.turbulence"
        );
        let names: Vec<&str> = SimplexNoiseForceAtParticles::INPUTS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(
            names,
            vec![
                "in",
                "particles",
                "amplitude_modulator",
                "amplitude",
                "modulator_gain",
                "z",
                "noise_scale",
                "active_count",
            ]
        );
        assert_eq!(
            SimplexNoiseForceAtParticles::INPUTS[0].ty,
            PortType::Array(vec2_layout)
        );
        assert!(SimplexNoiseForceAtParticles::INPUTS[0].required);
        assert_eq!(
            SimplexNoiseForceAtParticles::INPUTS[1].ty,
            PortType::Array(particle_layout)
        );
        assert!(SimplexNoiseForceAtParticles::INPUTS[1].required);
        assert_eq!(
            SimplexNoiseForceAtParticles::INPUTS[2].ty,
            PortType::Texture2D
        );
        assert!(!SimplexNoiseForceAtParticles::INPUTS[2].required);

        assert_eq!(SimplexNoiseForceAtParticles::OUTPUTS.len(), 1);
        assert_eq!(
            SimplexNoiseForceAtParticles::OUTPUTS[0].ty,
            PortType::Array(vec2_layout)
        );

        let prim = SimplexNoiseForceAtParticles::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn amplitude_z_and_noise_scale_port_shadow_params() {
        for name in ["amplitude", "z", "noise_scale", "modulator_gain"] {
            let has_port = SimplexNoiseForceAtParticles::INPUTS
                .iter()
                .any(|p| p.name == name);
            let has_param = SimplexNoiseForceAtParticles::PARAMS
                .iter()
                .any(|p| p.name == name);
            assert!(has_port, "input port '{name}' missing");
            assert!(has_param, "param '{name}' missing");
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SimplexNoiseForceAtParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(
            node.type_id().as_str(),
            "node.turbulence"
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain COINCIDENT + OPTIONAL-TEXTURE parity oracle (freeze §12).
    //! Two coincident array inputs (force [f32;2] + particles) plus an optional
    //! modulator Texture2D gated by an injected use_amplitude_modulator flag.
    //! The generated kernel must reproduce the hand kernel's in-place force add
    //! element-for-element in BOTH modulator modes. Hand binds particles@1/
    //! forces@2/tex@3/samp@4; generated binds force@1/particles@2/tex@3/samp@4/
    //! force@5 (in/out alias one force buffer).
    use super::*;
    use half::f16;
    use manifold_gpu::GpuTextureFormat as Fmt;

    fn modulator_tex(device: &manifold_gpu::GpuDevice, w: u32, h: u32) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i] = f16::from_f32((x + y) as f32 / (w + h) as f32); // R = density-ish field
                px[i + 3] = f16::from_f32(1.0);
            }
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: Fmt::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "snf-modulator-test",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(
                px.as_ptr().cast::<u8>(),
                std::mem::size_of_val(px.as_slice()),
            )
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_snf(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        particles: &[Particle],
        forces: &[[f32; 2]],
        modulator: &GpuTexture,
        uniform: &[u8],
        count: u32,
        generated: bool,
    ) -> Vec<[f32; 2]> {
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "snf-oracle");
        let pbuf = device.create_buffer_shared(std::mem::size_of_val(particles) as u64);
        let fbuf = device.create_buffer_shared(std::mem::size_of_val(forces) as u64);
        unsafe {
            pbuf.write(0, bytemuck::cast_slice(particles));
            fbuf.write(0, bytemuck::cast_slice(forces));
        }
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        // Hand binds particles@1/forces@2/tex@3/samp@4; generated follows INPUTS:
        // force@1/particles@2/tex@3/samp@4 plus the aliased force@5 (in/out).
        let bindings = if generated {
            vec![
                GpuBinding::Bytes { binding: 0, data: uniform },
                GpuBinding::Buffer { binding: 1, buffer: &fbuf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &pbuf, offset: 0 },
                GpuBinding::Texture { binding: 3, texture: modulator },
                GpuBinding::Sampler { binding: 4, sampler: &sampler },
                GpuBinding::Buffer { binding: 5, buffer: &fbuf, offset: 0 },
            ]
        } else {
            vec![
                GpuBinding::Bytes { binding: 0, data: uniform },
                GpuBinding::Buffer { binding: 1, buffer: &pbuf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &fbuf, offset: 0 },
                GpuBinding::Texture { binding: 3, texture: modulator },
                GpuBinding::Sampler { binding: 4, sampler: &sampler },
            ]
        };
        let mut enc = device.create_encoder("snf-oracle");
        enc.dispatch_compute(&pipeline, &bindings, [count.div_ceil(256), 1, 1], "snf-oracle");
        enc.commit_and_wait_completed();
        let ptr = fbuf.mapped_ptr().expect("shared force buffer");
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const [f32; 2], forces.len()) };
        slice.to_vec()
    }

    #[test]
    fn generated_snf_matches_hand_kernel_both_modulator_modes() {
        let device = crate::test_device();
        let modulator = modulator_tex(&device, 16, 16);
        let mk = |pos: [f32; 3], life: f32| Particle {
            position: pos,
            _pad0: 0.0,
            velocity: [0.0; 3],
            life,
            age: 0.0,
            _pad1: [0.0; 3],
            color: [0.0; 4],
        };
        let particles = [
            mk([0.2, 0.3, 0.0], 1.0),
            mk([0.7, 0.6, 0.0], 1.0),
            mk([0.9, 0.1, 0.0], 1.0),
            mk([0.5, 0.5, 0.0], 0.0), // dead → force unchanged
        ];
        let forces = [[0.01f32, -0.02], [0.0, 0.0], [-0.05, 0.03], [0.04, 0.04]];
        let n = particles.len() as u32;
        let amplitude = 0.01f32;
        let modulator_gain = 2.0f32;
        let z = 3.5f32;
        let noise_scale = 2.0f32;

        for has_mod in [0u32, 1u32] {
            // Hand layout: active_count(u32), amplitude(f32), modulator_gain(f32),
            //   z(f32), noise_scale(f32), has_modulator(u32), 2 pad.
            let mut hand = Vec::new();
            hand.extend_from_slice(&n.to_le_bytes());
            hand.extend_from_slice(&amplitude.to_le_bytes());
            hand.extend_from_slice(&modulator_gain.to_le_bytes());
            hand.extend_from_slice(&z.to_le_bytes());
            hand.extend_from_slice(&noise_scale.to_le_bytes());
            hand.extend_from_slice(&has_mod.to_le_bytes());
            hand.extend_from_slice(&[0u8; 8]);

            // Generated layout: amplitude(f32), modulator_gain(f32), z(f32),
            //   noise_scale(f32), active_count(i32), use_amplitude_modulator(u32),
            //   dispatch_count(u32), 1 pad.
            let mut gen_bytes = Vec::new();
            gen_bytes.extend_from_slice(&amplitude.to_le_bytes());
            gen_bytes.extend_from_slice(&modulator_gain.to_le_bytes());
            gen_bytes.extend_from_slice(&z.to_le_bytes());
            gen_bytes.extend_from_slice(&noise_scale.to_le_bytes());
            gen_bytes.extend_from_slice(&(n as i32).to_le_bytes());
            gen_bytes.extend_from_slice(&has_mod.to_le_bytes());
            gen_bytes.extend_from_slice(&n.to_le_bytes());
            gen_bytes.extend_from_slice(&[0u8; 4]);

            let hand_wgsl = include_str!("shaders/simplex_noise_force_at_particles.wgsl");
            let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<
                SimplexNoiseForceAtParticles,
            >()
            .expect("simplex_noise_force_at_particles buffer codegen");
            assert!(
                gen_wgsl.contains("use_amplitude_modulator: u32"),
                "optional-texture use flag injected"
            );

            let from_hand =
                dispatch_snf(&device, hand_wgsl, &particles, &forces, &modulator, &hand, n, false);
            let from_gen = dispatch_snf(
                &device, &gen_wgsl, &particles, &forces, &modulator, &gen_bytes, n, true,
            );

            for i in 0..forces.len() {
                for c in 0..2 {
                    assert!(
                        (from_hand[i][c] - from_gen[i][c]).abs() < 1e-6,
                        "has_mod={has_mod} force {i}[{c}]: hand={} gen={}",
                        from_hand[i][c],
                        from_gen[i][c]
                    );
                }
            }
        }
    }
}
