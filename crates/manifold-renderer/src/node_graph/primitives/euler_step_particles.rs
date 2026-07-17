//! `node.move_particles` — apply a per-particle 2D force to
//! each live particle's position via one Euler step.
//!
//! `position.xy += forces[i] * speed * dt_frame_normalized`
//!
//! where `dt_frame_normalized = ctx.time.delta * 60` so the same
//! `speed` knob gives consistent visual motion at any frame rate
//! (matches the legacy `fluid_simulate`'s `dt_scale = dt * 60`
//! convention).
//!
//! Dead particles (`life <= 0`) pass through unchanged. No boundary
//! handling — pair with `node.wrap_around` (or a future
//! `boundary_death` atom) for the position-bounds policy.

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order
/// (`active_count` Int → i32, `speed` f32), then the derived `dt_scaled`
/// (= delta*60, declared `derived_uniforms` — not a param), then the
/// codegen-injected `dispatch_count` element count. 4 words = 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EulerUniforms {
    active_count: i32,
    speed: f32,
    dt_scaled: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: EulerStepParticles,
    type_id: "node.move_particles",
    purpose: "Apply one Euler integration step to each live particle's position.xy by a per-particle 2D force. `position.xy += forces[i] * speed * (delta * 60)`. Frame-rate-normalised via the `* 60` scale so the same `speed` value gives consistent motion across frame rates (matches the legacy fluid_simulate's `dt_scale = dt * 60`). Dead particles (life <= 0) pass through unchanged. No boundary handling — pair with `node.wrap_around` for the toroidal policy.",
    inputs: {
        in: Array(Particle) required,
        forces: Array([f32; 2]) required,
        speed: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
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
        ParamDef {
            name: Cow::Borrowed("speed"),
            label: "Speed",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Aliased in/out — the dispatch mutates particles in place and the chain build resolves `in` and `out` to one physical buffer. `speed` is port-shadow so a control wire (LFO, audio band, manual slider) drives the advection energy. Typical chain: `sample_texture_at_particles → euler_step_particles → wrap_particles_torus`.",
    examples: [],
    picker: { label: "Move Particles (Euler step)", category: Atom },
    summary: "Moves every particle one step along its velocity each frame. The basic integrator that makes a particle system actually move.",
    category: Particles2D,
    role: Filter,
    aliases: ["move particles", "euler step particles", "integrate", "step", "euler"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/euler_step_particles_body.wgsl"),
    derived_uniforms: ["dt_scaled"],
}

// D7/P0 (`docs/CINEMATIC_POST_DESIGN.md`): registers this atom's per-frame
// recompute for the `dt_scaled` derived uniform, so a FUSED region containing
// this atom can refresh the field every frame (`node.wgsl_compute::evaluate`)
// instead of the deleted install-time `system.generator_input` control wire.
// Matches `run()`'s own `dt_scaled` computation below exactly.
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.move_particles",
        recompute: |ctx| Some(vec![ctx.frame.delta.0 as f32 * 60.0]),
    }
}

impl Primitive for EulerStepParticles {
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
        let active_count = ctx.scalar_or_param("active_count", 100_000.0).round().max(0.0) as u32;
        let speed = ctx.scalar_or_param("speed", 1.0);
        let dt_scaled = ctx.time.delta.0 as f32 * 60.0;

        let Some(particles) = ctx.inputs.array("in") else {
            return;
        };
        let Some(forces) = ctx.inputs.array("forces") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let _ = out;

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (particles.size / particle_size) as u32;
        let active_count = active_count.min(capacity);
        if active_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident path, derived dt_scaled). euler_step_particles.wgsl is
            // the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.move_particles standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.move_particles",
            )
        });

        let uniforms = EulerUniforms {
            active_count: active_count as i32,
            speed,
            dt_scaled,
            dispatch_count: active_count,
        };

        // `in`/`out` alias one particle buffer (aliased_array_io). The generated
        // kernel binds buf_in (read, 1), buf_forces (read, 2), buf_out
        // (read_write, 3); bind the particle buffer to BOTH 1 and 3. Pointwise so
        // the aliasing is race-free.
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
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: forces,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: particles,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.move_particles",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_aliased_particle_in_out_and_vec2_forces() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();
        let vec2_layout = ArrayType::of_known::<[f32; 2]>();

        assert_eq!(EulerStepParticles::TYPE_ID, "node.move_particles");
        let names: Vec<&str> = EulerStepParticles::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["in", "forces", "speed", "active_count"]);
        assert_eq!(
            EulerStepParticles::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert!(EulerStepParticles::INPUTS[0].required);
        assert_eq!(
            EulerStepParticles::INPUTS[1].ty,
            PortType::Array(vec2_layout)
        );
        assert!(EulerStepParticles::INPUTS[1].required);

        assert_eq!(EulerStepParticles::OUTPUTS.len(), 1);
        assert_eq!(
            EulerStepParticles::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );

        let prim = EulerStepParticles::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn speed_port_shadows_param() {
        let has_port = EulerStepParticles::INPUTS.iter().any(|p| p.name == "speed");
        let has_param = EulerStepParticles::PARAMS.iter().any(|p| p.name == "speed");
        assert!(has_port);
        assert!(has_param);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = EulerStepParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.move_particles");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain parity oracle (freeze §12) — the hardest coincident case:
    //! TWO coincident array inputs (Particle + [f32;2] forces), a DERIVED uniform
    //! field (dt_scaled = delta*60, not a param), and ALIASED in/out. The
    //! generated kernel must reproduce the hand `euler_step_particles.wgsl`
    //! particle-for-particle through the real aliased binding (the particle
    //! buffer bound to both the read slot (1) and read_write slot (3)).
    use super::*;

    /// Dispatch an euler kernel over `particles` + `forces` and read the
    /// (in-place) particle buffer back. `is_generated` adds the generated
    /// kernel's aliased buf_out at binding 3 (the hand kernel has only 3 slots).
    fn dispatch_euler(
        wgsl: &str,
        particles: &[Particle],
        forces: &[[f32; 2]],
        uniform: &[u8],
        count: u32,
        is_generated: bool,
    ) -> Vec<Particle> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "euler-oracle");
        let p_buf = device.create_buffer_shared(std::mem::size_of_val(particles) as u64);
        let f_buf = device.create_buffer_shared(std::mem::size_of_val(forces) as u64);
        unsafe {
            p_buf.write(0, bytemuck::cast_slice(particles));
            f_buf.write(0, bytemuck::cast_slice(forces));
        }
        let mut bindings = vec![
            GpuBinding::Bytes { binding: 0, data: uniform },
            GpuBinding::Buffer { binding: 1, buffer: &p_buf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &f_buf, offset: 0 },
        ];
        if is_generated {
            bindings.push(GpuBinding::Buffer { binding: 3, buffer: &p_buf, offset: 0 });
        }
        let mut enc = device.create_encoder("euler-oracle");
        enc.dispatch_compute(&pipeline, &bindings, [count.div_ceil(256), 1, 1], "euler-oracle");
        enc.commit_and_wait_completed();
        let ptr = p_buf.mapped_ptr().expect("shared particle buffer");
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const Particle, particles.len()) };
        slice.to_vec()
    }

    /// Generated euler reproduces the hand kernel: live particles step by
    /// `forces[i] * speed * dt_scaled` (z forced to 0), dead particles pass
    /// through, velocity/age/color carry through. Bit-identical (same WGSL ops).
    #[test]
    fn generated_euler_matches_hand_kernel() {
        let mk = |pos: [f32; 3], life: f32| Particle {
            position: pos,
            _pad0: 0.0,
            velocity: [0.1, -0.2, 0.3],
            life,
            age: 0.5,
            _pad1: [0.0; 3],
            color: [0.2, 0.4, 0.6, 1.0],
        };
        let particles = [
            mk([0.30, 0.70, 0.0], 1.0),
            mk([0.45, 0.10, 0.0], 1.0),
            mk([0.90, 0.50, 0.0], 0.5),
            mk([0.50, 0.50, 0.0], 0.0), // dead → unchanged
        ];
        let forces: [[f32; 2]; 4] = [[0.02, -0.05], [-0.10, 0.03], [0.07, 0.07], [0.99, 0.99]];
        let n = particles.len() as u32;
        let speed = 2.0f32;
        let dt_scaled = 0.5f32;

        // Hand layout: active_count(u32), speed(f32), dt_scaled(f32), pad.
        let mut hand = Vec::new();
        hand.extend_from_slice(&n.to_le_bytes());
        hand.extend_from_slice(&speed.to_le_bytes());
        hand.extend_from_slice(&dt_scaled.to_le_bytes());
        hand.extend_from_slice(&0u32.to_le_bytes());

        // Generated layout: active_count(i32), speed(f32), dt_scaled(f32), dispatch_count(u32).
        let mut gen_bytes = Vec::new();
        gen_bytes.extend_from_slice(&(n as i32).to_le_bytes());
        gen_bytes.extend_from_slice(&speed.to_le_bytes());
        gen_bytes.extend_from_slice(&dt_scaled.to_le_bytes());
        gen_bytes.extend_from_slice(&n.to_le_bytes());

        let hand_wgsl = include_str!("shaders/euler_step_particles.wgsl");
        let gen_wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<EulerStepParticles>()
                .expect("euler_step_particles buffer codegen");
        assert!(gen_wgsl.contains("dt_scaled: f32"), "derived field in the uniform");
        assert!(gen_wgsl.contains("struct Element2"), "force-pair struct synthesized");
        assert!(gen_wgsl.contains("let e_forces = buf_forces[idx];"), "forces pre-read");

        let from_hand = dispatch_euler(hand_wgsl, &particles, &forces, &hand, n, false);
        let from_gen = dispatch_euler(&gen_wgsl, &particles, &forces, &gen_bytes, n, true);

        for i in 0..particles.len() {
            for c in 0..3 {
                assert!(
                    (from_hand[i].position[c] - from_gen[i].position[c]).abs() < 1e-6,
                    "particle {i} position[{c}]: hand={} gen={}",
                    from_hand[i].position[c],
                    from_gen[i].position[c]
                );
                assert!(
                    (from_hand[i].velocity[c] - from_gen[i].velocity[c]).abs() < 1e-6,
                    "particle {i} velocity[{c}] passthrough"
                );
            }
            assert!((from_hand[i].life - from_gen[i].life).abs() < 1e-6, "particle {i} life");
        }
    }
}
