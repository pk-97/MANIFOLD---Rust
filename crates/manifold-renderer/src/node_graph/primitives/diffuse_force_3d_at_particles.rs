//! `node.spread_out_3d` — per-particle incoherent 3D
//! random kick added in place to an `Array<[f32; 3]>` force buffer,
//! weighted by local density.
//!
//! Bit-exact with the per-particle diffusion step of the legacy fused
//! `node.fluid_simulate_3d`:
//!
//! ```text
//! capped     = density.r / (1 + density.r) at p.position
//! diff_seed  = i * 1664525 + frame_count * 747796405
//! forces[i] += (hash_float3(diff_seed) - 0.5) * diffusion * capped
//! ```
//!
//! Incoherent (per-particle hash, reseeded each frame) where
//! `node.turbulence_3d` is spatially coherent. The
//! density weighting concentrates the kick where particles have clumped,
//! so it doubles as an anti-clumping diffusion. Sibling on the velocity
//! field would be `node.spread_out` (attractor sims); this
//! one adds to the force buffer so the kick is integrated through
//! `speed * dt` by `node.move_particles_3d`.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`diffusion`
/// f32, `active_count` Int → i32) then the derived `frame_count` (u32, exact
/// integer seed) then the codegen-injected `dispatch_count`. 4 words = 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DiffuseUniforms {
    diffusion: f32,
    active_count: i32,
    frame_count: u32,
    dispatch_count: u32,
}

crate::primitive! {
    name: DiffuseForce3DAtParticles,
    type_id: "node.spread_out_3d",
    purpose: "Per-particle incoherent 3D random kick added in-place to an Array<[f32; 3]> force buffer, weighted by local density. forces[i] += (hash_float3(i, frame) - 0.5) * diffusion * capped(density). Reseeds the hash each frame (Brownian, not drift); the density weighting concentrates the kick where particles clump (anti-clumping diffusion). Decomposed from the diffusion step of the fused node.fluid_simulate_3d.",
    inputs: {
        in: Array([f32; 3]) required,
        particles: Array(Particle) required,
        density: Texture3D required,
        diffusion: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array([f32; 3]),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("diffusion"),
            label: "Diffusion",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0166),
            range: Some((0.0, 0.5)),
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
    composition_notes: "Aliased Array<[f32; 3]> in/out (one force buffer, in-place add). `diffusion` is port-shadow so a control wire drives the kick energy live. The density Texture3D weights the kick by `capped(d) = d/(1+d)` — particles in dense regions get a stronger random push, which spreads clumps. Early-outs when diffusion <= 0. Wire between node.turbulence_3d and node.move_particles_3d so the kick is integrated through speed*dt.",
    examples: ["FluidSim3D"],
    picker: { label: "Spread Out (3D diffuse)", category: Atom },
    summary: "Gives each 3D particle a small random kick so a tight clump slowly spreads apart in space.",
    category: Particles3D,
    role: Filter,
    aliases: ["spread out 3d", "diffuse force 3d at particles", "diffuse 3d", "jitter"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/diffuse_force_3d_at_particles_body.wgsl"),
    derived_uniforms: ["frame_count:u32"],
}

// D7/P0 (`docs/CINEMATIC_POST_DESIGN.md`): per-frame recompute for a FUSED
// region's `frame_count` field. Matches `run()`'s own computation below
// exactly; `wgsl_compute`'s pack step casts through the field's real
// `UniformMemberType::U32` (`.max(0.0) as u32`), so this stays an exact
// integer the same way the standalone path always has.
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.spread_out_3d",
        recompute: |ctx| Some(vec![ctx.frame.frame_count as f32]),
    }
}

impl Primitive for DiffuseForce3DAtParticles {
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
        let diffusion = ctx.scalar_or_param("diffusion", 0.0166);
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

        let frame_count = ctx.time.frame_count as u32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident multi-input + Texture3D + derived frame_count).
            // diffuse_force_3d_at_particles.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.spread_out_3d standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.spread_out_3d",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = DiffuseUniforms {
            diffusion,
            active_count: active_count as i32,
            frame_count,
            dispatch_count: active_count,
        };

        // Generated binding order follows INPUTS: `in` (force) → buf_in(1),
        // `particles` → buf_particles(2), `density` texture → (3), sampler → (4),
        // output → buf_out(5). `in`/`out` alias the force buffer → bind it to both
        // 1 and 5.
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
            "node.spread_out_3d",
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
        let vec3_layout = ArrayType::of_known::<[f32; 3]>();

        assert_eq!(
            DiffuseForce3DAtParticles::TYPE_ID,
            "node.spread_out_3d"
        );
        let names: Vec<&str> = DiffuseForce3DAtParticles::INPUTS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(
            names,
            vec!["in", "particles", "density", "diffusion", "active_count"]
        );
        assert_eq!(
            DiffuseForce3DAtParticles::INPUTS[0].ty,
            PortType::Array(vec3_layout)
        );
        assert!(DiffuseForce3DAtParticles::INPUTS[0].required);
        assert_eq!(DiffuseForce3DAtParticles::INPUTS[2].ty, PortType::Texture3D);
        assert!(DiffuseForce3DAtParticles::INPUTS[2].required);

        assert_eq!(DiffuseForce3DAtParticles::OUTPUTS.len(), 1);
        assert_eq!(
            DiffuseForce3DAtParticles::OUTPUTS[0].ty,
            PortType::Array(vec3_layout)
        );

        let prim = DiffuseForce3DAtParticles::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn diffusion_port_shadows_param() {
        let has_port = DiffuseForce3DAtParticles::INPUTS
            .iter()
            .any(|p| p.name == "diffusion");
        let has_param = DiffuseForce3DAtParticles::PARAMS
            .iter()
            .any(|p| p.name == "diffusion");
        assert!(has_port);
        assert!(has_param);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = DiffuseForce3DAtParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(
            node.type_id().as_str(),
            "node.spread_out_3d"
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain multi-input coincident + Texture3D + derived parity oracle
    //! (freeze §12). The generated kernel reads force `in` + particles, samples
    //! the density Texture3D at the particle position, adds the density-weighted
    //! hash kick to the force (aliased). Must reproduce the hand kernel force-for-
    //! force. Hand binds particles@1/forces@2/density@3/samp@4; generated follows
    //! INPUTS: force@1/particles@2/density@3/samp@4/force@5.
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
                    // R = a density-ish field in [0,1].
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
            label: "diffuse3d-density-test",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    fn dispatch_diffuse3d(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        forces: &[[f32; 3]],
        particles: &[Particle],
        density: &GpuTexture,
        uniform: &[u8],
        count: u32,
        generated: bool,
    ) -> Vec<[f32; 3]> {
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "diffuse3d-oracle");
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
        let mut enc = device.create_encoder("diffuse3d-oracle");
        enc.dispatch_compute(&pipeline, &bindings, [count.div_ceil(256), 1, 1], "diffuse3d-oracle");
        enc.commit_and_wait_completed();
        let ptr = f_buf.mapped_ptr().expect("shared force buffer");
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const [f32; 3], forces.len()) };
        slice.to_vec()
    }

    #[test]
    fn generated_diffuse3d_matches_hand_kernel() {
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
        let diffusion = 0.05f32;
        let frame_count = 91u32;

        // Hand layout: active_count(u32), frame_count(u32), diffusion(f32), pad.
        let mut hand = Vec::new();
        hand.extend_from_slice(&n.to_le_bytes());
        hand.extend_from_slice(&frame_count.to_le_bytes());
        hand.extend_from_slice(&diffusion.to_le_bytes());
        hand.extend_from_slice(&0u32.to_le_bytes());

        // Generated layout: diffusion(f32), active_count(i32), frame_count(u32), dispatch_count(u32).
        let mut gen_bytes = Vec::new();
        gen_bytes.extend_from_slice(&diffusion.to_le_bytes());
        gen_bytes.extend_from_slice(&(n as i32).to_le_bytes());
        gen_bytes.extend_from_slice(&frame_count.to_le_bytes());
        gen_bytes.extend_from_slice(&n.to_le_bytes());

        let hand_wgsl = include_str!("shaders/diffuse_force_3d_at_particles.wgsl");
        let gen_wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<DiffuseForce3DAtParticles>()
                .expect("diffuse_force_3d_at_particles buffer codegen");

        let from_hand =
            dispatch_diffuse3d(&device, hand_wgsl, &forces, &particles, &density, &hand, n, false);
        let from_gen =
            dispatch_diffuse3d(&device, &gen_wgsl, &forces, &particles, &density, &gen_bytes, n, true);

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
