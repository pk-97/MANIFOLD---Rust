//! `node.spread_out` — hash-based random kick on the 3D
//! state of each particle.
//!
//! Generic Brownian-noise atom for any Array<Particle> pipeline whose
//! consumer treats `velocity` as a 3D state field. The bundled
//! `integrate_particles_attractor` had this folded into its simulate
//! shader as `if u.diffusion > 0.0 { state += hash_kick }`; pulling
//! it out gives the JSON graph a knob the user can wire to an LFO or
//! audio band, and lets future particle effects (fluid sims, sparks,
//! swarms) compose the same kick without re-implementing it. The
//! position-domain sibling for fluid sims is `diffuse_force_3d_at_particles`
//! (kicks the force buffer, density-weighted) / `anti_clump_particles`.
//!
//! Aliased `in`/`out` (single physical buffer, in-place mutation) —
//! same shape as `node.integrate_particles` and the rest of the
//! particle-pipeline atoms.

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use std::borrow::Cow;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`diffusion`
/// f32, `active_count` Int → i32), then the derived `frame_count` (u32, an exact
/// integer seed — declared `derived_uniforms: ["frame_count:u32"]`), then the
/// codegen-injected `dispatch_count`. 4 words = 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DiffuseUniforms {
    diffusion: f32,
    active_count: i32,
    frame_count: u32,
    dispatch_count: u32,
}

crate::primitive! {
    name: ArrayDiffuseParticles,
    type_id: "node.spread_out",
    purpose: "Apply a per-particle hash-based random kick to `Particle.velocity`. One GPU dispatch over Array<Particle> with aliased read+write. `diffusion` scales the kick magnitude (typical range 0..0.05); zero means no-op. `frame_count` reseeds the hash each frame so the kick is genuinely uncorrelated across frames. Generic Brownian-noise atom — pairs with any particle integrator (attractor ODE, fluid sim, advection) that wants additive jitter on its 3D state.",
    inputs: {
        in: Array(Particle) required,
        diffusion: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("diffusion"),
            label: "Diffusion",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 0.05)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("active_count"),
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(500_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire after the integrator's primary state update. `diffusion` accepts a control wire (LFO / audio band / driver) for live-modulated jitter. The hash seed combines particle id with frame_count so adjacent frames produce independent kicks (not a slow drift). Diffusion = 0 still dispatches but the shader early-outs after the count check — cheap when unused. Aliased in/out: single physical buffer, in-place mutation, downstream consumers see the diffused state on the same frame.",
    examples: [],
    picker: { label: "Spread Out (diffuse)", category: Atom },
    summary: "Gives each particle a small random kick so a tight clump slowly spreads apart. Adds a bit of life and scatter.",
    category: Particles2D,
    role: Filter,
    aliases: ["spread out", "array diffuse particles", "diffuse", "jitter", "random kick"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/array_diffuse_particles_body.wgsl"),
    derived_uniforms: ["frame_count:u32"],
}

// D7/P0 (`docs/CINEMATIC_POST_DESIGN.md`): per-frame recompute for a FUSED
// region's `frame_count` field. Matches `run()`'s own computation below.
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.spread_out",
        recompute: |ctx| Some(vec![ctx.frame.frame_count as f32]),
    }
}

impl Primitive for ArrayDiffuseParticles {
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
        let diffusion = ctx.scalar_or_param("diffusion", 0.0);
        let active_count = ctx.scalar_or_param("active_count", 500_000.0).round().max(0.0) as u32;

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let _ = out_buf;

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (in_buf.size / particle_size) as u32;
        let active_count = active_count.min(capacity);
        if active_count == 0 {
            return;
        }

        let frame_count = ctx.time.frame_count as u32;

        let uniforms = DiffuseUniforms {
            diffusion,
            active_count: active_count as i32,
            frame_count,
            dispatch_count: active_count,
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident path; derived frame_count:u32). array_diffuse_particles.wgsl
            // (the hand-kernel parity oracle) was deleted 2026-07-20 (W1-B,
            // migration scaffolding retired).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.spread_out standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.spread_out",
            )
        });

        // `in`/`out` alias one particle buffer; the generated kernel binds buf_in
        // (read, 1) + buf_out (read_write, 2) — bind it to both (pointwise).
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: in_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: in_buf,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.spread_out",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn diffuse_declares_aliased_particle_in_and_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();
        assert_eq!(ArrayDiffuseParticles::TYPE_ID, "node.spread_out");

        let in_port = ArrayDiffuseParticles::INPUTS
            .iter()
            .find(|p| p.name == "in")
            .expect("`in` port");
        assert_eq!(in_port.ty, PortType::Array(particle_layout));
        assert!(in_port.required);

        assert_eq!(ArrayDiffuseParticles::OUTPUTS.len(), 1);
        assert_eq!(ArrayDiffuseParticles::OUTPUTS[0].name, "out");
        assert_eq!(
            ArrayDiffuseParticles::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );

        let prim = ArrayDiffuseParticles::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn diffusion_and_active_count_port_shadow_params() {
        for name in ["diffusion", "active_count"] {
            let has_port = ArrayDiffuseParticles::INPUTS
                .iter()
                .any(|p| p.name == name);
            let has_param = ArrayDiffuseParticles::PARAMS
                .iter()
                .any(|p| p.name == name);
            assert!(has_port, "{name} must have a port-shadow input");
            assert!(has_param, "{name} must have a param");
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ArrayDiffuseParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.spread_out");
    }
}

