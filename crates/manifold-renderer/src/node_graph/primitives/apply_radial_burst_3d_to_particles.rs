//! `node.add_burst_3d` — per-particle 3D injection
//! burst around one of four hardcoded tetrahedron-vertex zones.
//!
//! The 3D sibling of `node.add_burst`. Where the
//! 2D atom pushes around a free `(point_x, point_y)`, the 3D burst
//! selects one of four fixed tetrahedron-vertex zones via `inject_index`
//! (-1 = off) and applies a noise-perturbed radial push plus a
//! vortex-ring tangent directly to `position.xyz`. Bit-exact with the
//! injection step of the legacy fused `node.fluid_simulate_3d`.
//!
//! `dt = delta * 60` is baked in (frame-rate-normalised like
//! `node.move_particles_3d`); time drives the noise-perturbation
//! phase. Place last in the per-particle position chain.

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use std::borrow::Cow;

/// Generated-codegen uniform layout: scalar params in PARAMS order
/// (`inject_index` Int → i32, `inject_force` f32, `inject_phase` f32,
/// `active_count` Int → i32), then the TWO derived fields (`time2` = seconds,
/// `dt_scaled` = delta*60), then the codegen-injected `dispatch_count`, padded
/// to a 16-byte multiple. 7 words + 1 pad = 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Burst3DUniforms {
    inject_index: i32,
    inject_force: f32,
    inject_phase: f32,
    active_count: i32,
    time2: f32,
    dt_scaled: f32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: ApplyRadialBurst3DToParticles,
    type_id: "node.add_burst_3d",
    purpose: "Per-particle 3D injection burst around one of four hardcoded tetrahedron-vertex zones. inject_index < 0 disables it; 0..3 selects a zone. Applies a noise-perturbed radial push + vortex-ring tangent (within radius 0.25, quartic falloff, attack/decay envelope from inject_phase) directly to position.xyz. The 3D sibling of node.add_burst; decomposed from the injection step of the fused node.fluid_simulate_3d.",
    inputs: {
        in: Array(Particle) required,
        inject_index: ScalarF32 optional,
        inject_force: ScalarF32 optional,
        inject_phase: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("inject_index"),
            label: "Inject Zone",
            ty: ParamType::Int,
            default: ParamValue::Float(-1.0),
            range: Some((-1.0, 3.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("inject_force"),
            label: "Inject Force",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("inject_phase"),
            label: "Inject Phase",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
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
    composition_notes: "Aliased in/out — mutates the particle buffer in place. `inject_index = -1` disables the burst (the kernel early-outs); the four zones (0..3) are hardcoded tetrahedron-vertex positions. Typical wiring: a clip-trigger cycle picks the zone, gated by node.inject_burst's `active` (mux to -1 when idle); `inject_force` from the gated force slider; `inject_phase` from inject_burst's phase. `dt = delta * 60` baked in; time uses ctx.time.seconds for the noise-perturbation phase. When inject_index < 0 or inject_force * envelope is tiny the kernel early-outs (cheap when idle).",
    examples: ["FluidSim3D"],
    picker: { label: "Add Burst (3D, radial)", category: Atom },
    summary: "Injects 3D particles in a burst around one of a few fixed zones, puffing new material into a 3D sim on a hit.",
    category: Particles3D,
    role: Filter,
    aliases: ["add burst 3d", "apply radial burst 3d to particles", "explosion 3d", "inject"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/apply_radial_burst_3d_to_particles_body.wgsl"),
    derived_uniforms: ["time2", "dt_scaled"],
}

// D7/P0 (`docs/CINEMATIC_POST_DESIGN.md`): per-frame recompute for a FUSED
// region's `time2`/`dt_scaled` fields, IN DECLARATION ORDER — `run()`'s own
// computation below.
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.add_burst_3d",
        recompute: |ctx| Some(vec![ctx.frame.seconds.0 as f32, ctx.frame.delta.0 as f32 * 60.0]),
    }
}

impl Primitive for ApplyRadialBurst3DToParticles {
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
        let inject_index = ctx.scalar_or_param("inject_index", -1.0).round() as i32;
        let inject_force = ctx.scalar_or_param("inject_force", 0.0);
        let inject_phase = ctx.scalar_or_param("inject_phase", 0.0);
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
        // Idle skip: a negative zone index disables the burst and the push scales
        // by inject_force, so either makes every thread a no-op write-back — skip
        // the dispatch. The in/out alias means the executor's stale-output guard
        // must be told the buffer is intentionally retained.
        if active_count == 0 || inject_index < 0 || inject_force == 0.0 {
            ctx.mark_gpu_accessed();
            return;
        }

        let time2 = ctx.time.seconds.0 as f32;
        let dt_scaled = ctx.time.delta.0 as f32 * 60.0;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident path; two derived fields; bespoke simplex + zone consts
            // inlined). apply_radial_burst_3d_to_particles.wgsl (the hand-kernel parity oracle) was deleted 2026-07-20 (W1-B, migration scaffolding retired).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.add_burst_3d standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.add_burst_3d",
            )
        });

        let uniforms = Burst3DUniforms {
            inject_index,
            inject_force,
            inject_phase,
            active_count: active_count as i32,
            time2,
            dt_scaled,
            dispatch_count: active_count,
            _pad0: 0,
        };

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
            "node.add_burst_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_aliased_particle_in_out_and_port_shadow_inputs() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(
            ApplyRadialBurst3DToParticles::TYPE_ID,
            "node.add_burst_3d"
        );
        let names: Vec<&str> = ApplyRadialBurst3DToParticles::INPUTS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(
            names,
            vec!["in", "inject_index", "inject_force", "inject_phase", "active_count"]
        );
        assert_eq!(
            ApplyRadialBurst3DToParticles::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert!(ApplyRadialBurst3DToParticles::INPUTS[0].required);
        for input in &ApplyRadialBurst3DToParticles::INPUTS[1..] {
            assert!(!input.required, "{} should be optional", input.name);
        }

        assert_eq!(ApplyRadialBurst3DToParticles::OUTPUTS.len(), 1);
        assert_eq!(
            ApplyRadialBurst3DToParticles::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );

        let prim = ApplyRadialBurst3DToParticles::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn uniform_struct_is_32_bytes() {
        assert_eq!(std::mem::size_of::<Burst3DUniforms>(), 32);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ApplyRadialBurst3DToParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(
            node.type_id().as_str(),
            "node.add_burst_3d"
        );
    }
}

