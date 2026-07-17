//! `node.wrap_around` — per-particle toroidal wrap of
//! `position.xy` to `[0, 1]²` via `fract(position.xy + 1)`.
//!
//! The cyclic-boundary policy atom. Pair downstream of
//! `node.move_particles` for the legacy
//! `integrate_particles` boundary behaviour. Dead particles
//! (`life <= 0`) pass through unchanged.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order
/// (`active_count`, Int → i32) then the codegen-injected `dispatch_count`
/// element count, padded to 16 bytes. The dispatch guard and `active_count`
/// param carry the same value here (active_count IS the live count); the body
/// ignores both and wraps its own element.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WrapUniforms {
    active_count: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: WrapParticlesTorus,
    type_id: "node.wrap_around",
    purpose: "Per-particle toroidal wrap: position.xy = fract(position.xy + 1). The cyclic-boundary policy atom for fluid sims and any flow-driven particle pipeline whose domain is `[0, 1]²`. Dead particles (life <= 0) pass through unchanged. Decomposed out of the legacy fused `integrate_particles` kernel — kept separate so different boundary policies (boundary_death, wall_bounce) can ship as sibling atoms without forking the integrator.",
    inputs: {
        in: Array(Particle) required,
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
    ],
    depth_rule: Terminal,
    composition_notes: "Aliased in/out — operates on the particle buffer in place. Typical chain: `sample_texture_at_particles → euler_step_particles → wrap_particles_torus`. For alternative boundary policies, swap this node for a future `boundary_death` (excess particles die when leaving [0,1]²) or `wall_bounce` sibling.",
    examples: [],
    picker: { label: "Wrap Around (torus)", category: Atom },
    summary: "Wraps particles back to the opposite edge when they leave the frame, so the cloud loops seamlessly instead of escaping.",
    category: Particles2D,
    role: Filter,
    aliases: ["wrap around", "wrap particles torus", "torus", "loop", "tile"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/wrap_particles_torus_body.wgsl"),
}

impl Primitive for WrapParticlesTorus {
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

        let Some(particles) = ctx.inputs.array("in") else {
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
            // coincident path). wrap_particles_torus.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.wrap_around standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.wrap_around",
            )
        });

        let uniforms = WrapUniforms {
            active_count: active_count as i32,
            dispatch_count: active_count,
            _pad0: 0,
            _pad1: 0,
        };

        // `in`/`out` alias one physical buffer (aliased_array_io). The generated
        // kernel binds the input as `read` (1) and the output as `read_write`
        // (2); bind that single aliased buffer to both. Pointwise (read [idx],
        // write [idx]) so the aliasing is race-free.
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
                    buffer: particles,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.wrap_around",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_aliased_particle_in_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(WrapParticlesTorus::TYPE_ID, "node.wrap_around");
        let names: Vec<&str> = WrapParticlesTorus::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["in", "active_count"]);
        assert_eq!(
            WrapParticlesTorus::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert!(WrapParticlesTorus::INPUTS[0].required);

        assert_eq!(WrapParticlesTorus::OUTPUTS.len(), 1);
        assert_eq!(
            WrapParticlesTorus::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );

        let prim = WrapParticlesTorus::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = WrapParticlesTorus::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.wrap_around");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain coincident parity oracle (freeze §12). The GENERATED
    //! standalone kernel — built from `wrap_particles_torus_body.wgsl` by the
    //! buffer coincident path — must reproduce the hand `wrap_particles_torus.wgsl`
    //! particle-for-particle, including the dead-particle pass-through and the
    //! aliased in/out (the generated kernel's separate `read` + `read_write`
    //! bindings are both backed by ONE buffer, exactly as `run()` binds them).
    use super::*;

    /// Dispatch a wrap kernel over `input` and read the (in-place) buffer back.
    /// `alias_two_slots` mirrors the generated kernel's two bindings onto one
    /// buffer (production aliasing); the hand kernel binds a single slot.
    fn dispatch_wrap(
        wgsl: &str,
        input: &[Particle],
        uniform: &[u8],
        count: u32,
        alias_two_slots: bool,
    ) -> Vec<Particle> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "wrap-oracle");
        let bytes = std::mem::size_of_val(input) as u64;
        let buf = device.create_buffer_shared(bytes);
        unsafe {
            buf.write(0, bytemuck::cast_slice(input));
        }
        let mut bindings = vec![
            GpuBinding::Bytes { binding: 0, data: uniform },
            GpuBinding::Buffer { binding: 1, buffer: &buf, offset: 0 },
        ];
        if alias_two_slots {
            bindings.push(GpuBinding::Buffer { binding: 2, buffer: &buf, offset: 0 });
        }
        let mut enc = device.create_encoder("wrap-oracle");
        enc.dispatch_compute(&pipeline, &bindings, [count.div_ceil(256), 1, 1], "wrap-oracle");
        enc.commit_and_wait_completed();
        let ptr = buf.mapped_ptr().expect("shared buffer");
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const Particle, input.len()) };
        slice.to_vec()
    }

    /// Generated wrap reproduces the hand kernel: live particles wrap
    /// `position.xy` via `fract(.+1)` (including far-out-of-range), dead
    /// particles pass through untouched, and the non-position channels carry
    /// through. Same WGSL ops both ways → bit-identical.
    #[test]
    fn generated_wrap_matches_hand_kernel() {
        let mk = |pos: [f32; 3], life: f32| Particle {
            position: pos,
            _pad0: 0.0,
            velocity: [0.1, -0.2, 0.3],
            life,
            age: 0.5,
            _pad1: [0.0; 3],
            color: [0.2, 0.4, 0.6, 1.0],
        };
        let input = [
            mk([0.3, 0.7, 0.0], 1.0),   // live, in range
            mk([1.4, -0.2, 0.0], 1.0),  // live, out of range → fract wraps
            mk([2.9, 3.1, 0.0], 0.5),   // live, far out
            mk([0.5, 0.5, 0.0], 0.0),   // dead → unchanged
            mk([-0.1, 1.9, 0.0], -1.0), // dead negative
        ];
        let n = input.len() as u32;

        // Hand layout: active_count(u32) + 3 pad words.
        let mut hand = Vec::new();
        hand.extend_from_slice(&n.to_le_bytes());
        hand.extend_from_slice(&[0u8; 12]);

        // Generated layout: active_count(i32), dispatch_count(u32), 2 pad words.
        let mut gen_bytes = Vec::new();
        gen_bytes.extend_from_slice(&(n as i32).to_le_bytes());
        gen_bytes.extend_from_slice(&n.to_le_bytes());
        gen_bytes.extend_from_slice(&[0u8; 8]);

        let hand_wgsl = include_str!("shaders/wrap_particles_torus.wgsl");
        let gen_wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<WrapParticlesTorus>()
                .expect("wrap_particles_torus buffer codegen");
        assert!(
            gen_wgsl.contains("var<storage, read> buf_in"),
            "coincident input pre-read from a read storage array"
        );
        assert!(
            gen_wgsl.contains("var<storage, read_write> buf_out"),
            "output written to a read_write storage array"
        );
        assert!(gen_wgsl.contains("let e_in = buf_in[idx];"), "wrapper pre-reads the element");

        let from_hand = dispatch_wrap(hand_wgsl, &input, &hand, n, false);
        let from_gen = dispatch_wrap(&gen_wgsl, &input, &gen_bytes, n, true);

        for i in 0..input.len() {
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
            for c in 0..4 {
                assert!(
                    (from_hand[i].color[c] - from_gen[i].color[c]).abs() < 1e-6,
                    "particle {i} color[{c}] passthrough"
                );
            }
        }
    }
}
