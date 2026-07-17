//! `node.turbulence_3d` — per-particle 3D simplex
//! noise advection added in place to an `Array<[f32; 3]>` force buffer.
//!
//! The 3D sibling of `node.turbulence`. 3D noise
//! is built from `SimplexNoise2D` evaluated on three orthogonal planes
//! (yz / xz / xy), with density-adaptive amplitude
//! (`turbulence * (1 + capped(density) * anti_clump)`). Bit-exact with
//! the noise-advection step of the legacy fused `node.fluid_simulate_3d`.
//!
//! Aliased `in`/`out` `Array<[f32; 3]>`: a single physical force buffer,
//! mutated in place (the noise is *added* to whatever upstream force
//! atoms already deposited). Pair downstream of
//! `node.sample_volume_at_particles`.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`turbulence`,
/// `anti_clump`, `turb_scale` f32, `active_count` Int → i32) then the derived
/// `time2` (= seconds) then the codegen-injected `dispatch_count`, padded to a
/// 16-byte multiple. 6 words + 2 pad = 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NoiseUniforms {
    turbulence: f32,
    anti_clump: f32,
    turb_scale: f32,
    active_count: i32,
    time2: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: SimplexNoiseForce3DAtParticles,
    type_id: "node.turbulence_3d",
    purpose: "Per-particle 3D simplex noise advection added in-place to an Array<[f32; 3]> force buffer. 3D noise from SimplexNoise2D on three orthogonal planes (yz/xz/xy), density-adaptive amplitude (turbulence * (1 + capped(density) * anti_clump), capped = d/(1+d)). Samples a density Texture3D at the particle's position. Aliased force in/out — one physical buffer, in-place add. The 3D sibling of node.turbulence, decomposed from the fused node.fluid_simulate_3d.",
    inputs: {
        in: Array([f32; 3]) required,
        particles: Array(Particle) required,
        density: Texture3D required,
        turbulence: ScalarF32 optional,
        anti_clump: ScalarF32 optional,
        turb_scale: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array([f32; 3]),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("turbulence"),
            label: "Turbulence",
            ty: ParamType::Float,
            default: ParamValue::Float(0.001),
            range: Some((0.0, 0.05)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("anti_clump"),
            label: "Anti-Clump",
            ty: ParamType::Float,
            default: ParamValue::Float(20.0),
            range: Some((0.0, 60.0)),
            enum_values: &[],
        },
        // Noise lattice cells across the unit volume. Default = the legacy
        // baked constant, so existing saves render unchanged; the FluidSim3D
        // card retunes it (BUG-066: at 2.0 one cell reads as a quadrant).
        ParamDef {
            name: Cow::Borrowed("turb_scale"),
            label: "Turbulence Detail",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.5, 32.0)),
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
    composition_notes: "Aliased Array<[f32; 3]> in/out (one force buffer, in-place add). `turbulence` and `anti_clump` are port-shadow so an LFO / clip-trigger envelope / outer-card slider drives the noise energy and the density-adaptive boost live. The density Texture3D modulates amplitude: where particles have accumulated (high density), the noise amplitude rises by `1 + capped(d) * anti_clump`, which spreads clumps apart. Time animates the noise field through `time2 * 0.1`. Wire downstream of node.sample_volume_at_particles, upstream of node.move_particles_3d.",
    examples: ["FluidSim3D"],
    picker: { label: "Turbulence (3D, simplex)", category: Atom },
    summary: "Pushes 3D particles around with a flowing 3D noise field for organic, swirling motion through space.",
    category: Particles3D,
    role: Filter,
    aliases: ["turbulence 3d", "simplex noise force 3d at particles", "noise force 3d", "flow", "simplex"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/simplex_noise_force_3d_at_particles_body.wgsl"),
    derived_uniforms: ["time2"],
}

// D7/P0 (`docs/CINEMATIC_POST_DESIGN.md`): per-frame recompute for a FUSED
// region's `time2` field. Matches `run()`'s own computation below.
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.turbulence_3d",
        recompute: |ctx| Some(vec![ctx.frame.seconds.0 as f32]),
    }
}

impl Primitive for SimplexNoiseForce3DAtParticles {
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
        let turbulence = ctx.scalar_or_param("turbulence", 0.001);
        let anti_clump = ctx.scalar_or_param("anti_clump", 20.0);
        let turb_scale = ctx.scalar_or_param("turb_scale", 2.0);
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
        let Some(density) = ctx.inputs.texture_3d("density") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let _ = out;

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let particle_capacity = (particles.size / particle_size) as u32;
        let force_capacity = (in_forces.size / 12) as u32;
        let active_count = active_count.min(particle_capacity).min(force_capacity);
        if active_count == 0 {
            return;
        }

        let time2 = ctx.time.seconds.0 as f32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident multi-input + Texture3D + derived time2; bespoke simplex
            // inlined). simplex_noise_force_3d_at_particles.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.turbulence_3d standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.turbulence_3d",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = NoiseUniforms {
            turbulence,
            anti_clump,
            turb_scale,
            active_count: active_count as i32,
            time2,
            dispatch_count: active_count,
            _pad0: 0,
            _pad1: 0,
        };

        // Generated binding order follows INPUTS: `in` (force) → buf_in(1),
        // `particles` → buf_particles(2), `density` → (3), sampler → (4), output →
        // buf_out(5). `in`/`out` alias the force buffer → bind it to both 1 and 5.
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
                    texture: density,
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
            "node.turbulence_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_aliased_vec3_in_out_required_particles_and_density() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();
        let vec3_layout = ArrayType::of_known::<[f32; 3]>();

        assert_eq!(
            SimplexNoiseForce3DAtParticles::TYPE_ID,
            "node.turbulence_3d"
        );
        let names: Vec<&str> = SimplexNoiseForce3DAtParticles::INPUTS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(
            names,
            vec![
                "in",
                "particles",
                "density",
                "turbulence",
                "anti_clump",
                "turb_scale",
                "active_count"
            ]
        );
        assert_eq!(
            SimplexNoiseForce3DAtParticles::INPUTS[0].ty,
            PortType::Array(vec3_layout)
        );
        assert!(SimplexNoiseForce3DAtParticles::INPUTS[0].required);
        assert_eq!(
            SimplexNoiseForce3DAtParticles::INPUTS[1].ty,
            PortType::Array(particle_layout)
        );
        assert_eq!(SimplexNoiseForce3DAtParticles::INPUTS[2].ty, PortType::Texture3D);
        assert!(SimplexNoiseForce3DAtParticles::INPUTS[2].required);

        assert_eq!(SimplexNoiseForce3DAtParticles::OUTPUTS.len(), 1);
        assert_eq!(
            SimplexNoiseForce3DAtParticles::OUTPUTS[0].ty,
            PortType::Array(vec3_layout)
        );

        let prim = SimplexNoiseForce3DAtParticles::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn turbulence_and_anti_clump_port_shadow_params() {
        for name in ["turbulence", "anti_clump", "turb_scale"] {
            let has_port = SimplexNoiseForce3DAtParticles::INPUTS
                .iter()
                .any(|p| p.name == name);
            let has_param = SimplexNoiseForce3DAtParticles::PARAMS
                .iter()
                .any(|p| p.name == name);
            assert!(has_port, "input port '{name}' missing");
            assert!(has_param, "param '{name}' missing");
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SimplexNoiseForce3DAtParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(
            node.type_id().as_str(),
            "node.turbulence_3d"
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain multi-input coincident + Texture3D + derived parity oracle
    //! (freeze §12). The generated kernel reads force `in` + particles, samples
    //! the density Texture3D, adds the density-adaptive 3-plane simplex noise to
    //! the force (aliased). Must reproduce the hand kernel force-for-force. Hand
    //! binds particles@1/forces@2/density@3/samp@4; generated follows INPUTS:
    //! force@1/particles@2/density@3/samp@4/force@5.
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
                    px[i] = f16::from_f32((x + y + z) as f32 / (3 * (n - 1)) as f32);
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
            label: "snf3d-density-test",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_noise3d(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        forces: &[[f32; 3]],
        particles: &[Particle],
        density: &GpuTexture,
        uniform: &[u8],
        count: u32,
        generated: bool,
    ) -> Vec<[f32; 3]> {
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "snf3d-oracle");
        let f_buf = device.create_buffer_shared(std::mem::size_of_val(forces) as u64);
        let p_buf = device.create_buffer_shared(std::mem::size_of_val(particles) as u64);
        unsafe {
            f_buf.write(0, bytemuck::cast_slice(forces));
            p_buf.write(0, bytemuck::cast_slice(particles));
        }
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let mut bindings = vec![GpuBinding::Bytes { binding: 0, data: uniform }];
        if generated {
            bindings.push(GpuBinding::Buffer { binding: 1, buffer: &f_buf, offset: 0 });
            bindings.push(GpuBinding::Buffer { binding: 2, buffer: &p_buf, offset: 0 });
            bindings.push(GpuBinding::Texture { binding: 3, texture: density });
            bindings.push(GpuBinding::Sampler { binding: 4, sampler: &sampler });
            bindings.push(GpuBinding::Buffer { binding: 5, buffer: &f_buf, offset: 0 });
        } else {
            bindings.push(GpuBinding::Buffer { binding: 1, buffer: &p_buf, offset: 0 });
            bindings.push(GpuBinding::Buffer { binding: 2, buffer: &f_buf, offset: 0 });
            bindings.push(GpuBinding::Texture { binding: 3, texture: density });
            bindings.push(GpuBinding::Sampler { binding: 4, sampler: &sampler });
        }
        let mut enc = device.create_encoder("snf3d-oracle");
        enc.dispatch_compute(&pipeline, &bindings, [count.div_ceil(256), 1, 1], "snf3d-oracle");
        enc.commit_and_wait_completed();
        let ptr = f_buf.mapped_ptr().expect("shared force buffer");
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const [f32; 3], forces.len()) };
        slice.to_vec()
    }

    #[test]
    fn generated_noise3d_matches_hand_kernel() {
        let device = crate::test_device();
        let density = gradient_volume(&device, 8);
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
            mk([0.2, 0.3, 0.4], 1.0),
            mk([0.7, 0.6, 0.5], 1.0),
            mk([0.9, 0.1, 0.8], 1.0),
            mk([0.5, 0.5, 0.5], 0.0), // dead → unchanged
        ];
        let forces: [[f32; 3]; 4] =
            [[0.1, 0.2, 0.3], [-0.1, 0.0, 0.2], [0.05, 0.05, 0.05], [0.9, 0.9, 0.9]];
        let n = particles.len() as u32;
        // Non-default turb_scale so the new lattice-frequency uniform is
        // actually exercised by the parity comparison.
        let (turbulence, anti_clump, turb_scale, time2) = (0.02f32, 20.0f32, 5.0f32, 1.7f32);

        // Hand layout: active_count(u32), turbulence(f32), anti_clump(f32),
        //   time2(f32), turb_scale(f32), 3 pad.
        let mut hand = Vec::new();
        hand.extend_from_slice(&n.to_le_bytes());
        hand.extend_from_slice(&turbulence.to_le_bytes());
        hand.extend_from_slice(&anti_clump.to_le_bytes());
        hand.extend_from_slice(&time2.to_le_bytes());
        hand.extend_from_slice(&turb_scale.to_le_bytes());
        hand.extend_from_slice(&[0u8; 12]);

        // Generated layout: turbulence(f32), anti_clump(f32), turb_scale(f32),
        //   active_count(i32), time2(f32), dispatch_count(u32), 2 pad.
        let mut gen_bytes = Vec::new();
        gen_bytes.extend_from_slice(&turbulence.to_le_bytes());
        gen_bytes.extend_from_slice(&anti_clump.to_le_bytes());
        gen_bytes.extend_from_slice(&turb_scale.to_le_bytes());
        gen_bytes.extend_from_slice(&(n as i32).to_le_bytes());
        gen_bytes.extend_from_slice(&time2.to_le_bytes());
        gen_bytes.extend_from_slice(&n.to_le_bytes());
        gen_bytes.extend_from_slice(&[0u8; 8]);

        let hand_wgsl = include_str!("shaders/simplex_noise_force_3d_at_particles.wgsl");
        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<
            SimplexNoiseForce3DAtParticles,
        >()
        .expect("simplex_noise_force_3d_at_particles buffer codegen");

        let from_hand =
            dispatch_noise3d(&device, hand_wgsl, &forces, &particles, &density, &hand, n, false);
        let from_gen =
            dispatch_noise3d(&device, &gen_wgsl, &forces, &particles, &density, &gen_bytes, n, true);

        for i in 0..forces.len() {
            for c in 0..3 {
                assert!(
                    (from_hand[i][c] - from_gen[i][c]).abs() < 1e-6,
                    "force {i}[{c}]: hand={} gen={}",
                    from_hand[i][c],
                    from_gen[i][c]
                );
            }
        }
    }
}
