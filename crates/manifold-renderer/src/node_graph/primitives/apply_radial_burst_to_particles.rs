//! `node.add_burst` — per-particle radial
//! impulse around a point. Mutates `Particle.position.xy` directly.
//!
//! The per-particle counterpart to `node.explosion_force`.
//! Where the texture-domain atom paints a vec2 force field that
//! particles sample via bilinear interpolation, this atom evaluates
//! the radial+tangent math at each particle's exact position and
//! applies the push directly — matching the legacy fluid_simulate's
//! injection-burst behaviour without the per-pixel quantisation /
//! bilinear smoothing near the inject centre.
//!
//! Same math as `radial_burst_force_field` (radial direction +
//! tangent curl + noise-perturbed radial + `(1 - t²)²` falloff
//! envelope × phase envelope × amplitude), but evaluated and applied
//! per-particle. `dt_frame_normalized = delta * 60` is baked in so
//! the burst strength matches the legacy's `inject_force × dt_scale`
//! convention.

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use std::borrow::Cow;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`point_x`,
/// `point_y`, `amplitude`, `envelope`, `radius`, `active_count` Int → i32), then
/// the TWO derived fields (`time_val` = seconds, `dt_scaled` = delta*60), then
/// the codegen-injected `dispatch_count`, padded to a 16-byte multiple. 9 words +
/// 3 pad = 48 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BurstUniforms {
    point_x: f32,
    point_y: f32,
    amplitude: f32,
    envelope: f32,
    radius: f32,
    active_count: i32,
    time_val: f32,
    dt_scaled: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: ApplyRadialBurstToParticles,
    type_id: "node.add_burst",
    purpose: "Per-particle radial impulse around `(point_x, point_y)` — evaluates the radial + tangent + noise-perturbed-radial + falloff math at each particle's exact UV and applies the resulting push to `position.xy` directly. The per-particle counterpart to `node.explosion_force` (which paints the same math as a texture for downstream sampling). Use this atom when bilinear smoothing near the impulse centre would muddy the visible kick — fluid sims, sparks reacting to beat hits, particle-text inject events.",
    inputs: {
        in: Array(Particle) required,
        point_x: ScalarF32 optional,
        point_y: ScalarF32 optional,
        amplitude: ScalarF32 optional,
        envelope: ScalarF32 optional,
        radius: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("point_x"),
            label: "Point X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("point_y"),
            label: "Point Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("amplitude"),
            label: "Amplitude",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("envelope"),
            label: "Envelope",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("radius"),
            label: "Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(0.25),
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
    composition_notes: "Aliased in/out — mutates the particle buffer in place. Typical wiring: `node.inject_burst` produces (active, phase, point_x, point_y); wire point_x/point_y straight in, envelope from `active * envelope_decay(phase)` or compose attack/decay externally. When `amplitude * envelope == 0` the dispatch is skipped entirely, free when idle. `dt = delta × 60` is baked in (frame-rate-normalised like `euler_step_particles`). Time uses ctx.time.seconds for the per-particle noise perturbation phase.",
    examples: [],
    picker: { label: "Add Burst (radial)", category: Atom },
    summary: "Pushes particles outward from a point in a burst, like an explosion or shockwave on a hit.",
    category: Particles2D,
    role: Filter,
    aliases: ["add burst", "apply radial burst to particles", "explosion", "shockwave", "impulse"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/apply_radial_burst_to_particles_body.wgsl"),
    derived_uniforms: ["time_val", "dt_scaled"],
}

// D7/P0 (`docs/CINEMATIC_POST_DESIGN.md`): per-frame recompute for a FUSED
// region's `time_val`/`dt_scaled` fields, IN DECLARATION ORDER (matches
// `derived_uniforms` above) — `run()`'s own computation below.
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.add_burst",
        recompute: |ctx| Some(vec![ctx.frame.seconds.0 as f32, ctx.frame.delta.0 as f32 * 60.0]),
    }
}

impl Primitive for ApplyRadialBurstToParticles {
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

    // The bespoke inlined `arb_simplex_noise_2d` makes this body register-heavy:
    // fused into FluidSim2D's euler+wrap chain the combined kernel measured
    // 3.05 ms vs 2.43 ms for the three standalone dispatches (occupancy cliff).
    // Standalone it costs ~0.69 ms when firing and skips its dispatch when idle.
    fn fusion_register_heavy(&self) -> bool {
        true
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let point_x = ctx.scalar_or_param("point_x", 0.5);
        let point_y = ctx.scalar_or_param("point_y", 0.5);
        let amplitude = ctx.scalar_or_param("amplitude", 0.0);
        let envelope = ctx.scalar_or_param("envelope", 0.0);
        let radius = ctx.scalar_or_param("radius", 0.25);
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
        // Idle skip: the kernel's own first guard returns the particle unchanged
        // when `amplitude * envelope < 1e-4`, so the whole dispatch is a no-op —
        // skip it CPU-side with the identical threshold. The in/out alias means
        // the executor's stale-output guard must be told the buffer is
        // intentionally retained.
        if active_count == 0 || amplitude * envelope < 1.0e-4 {
            ctx.mark_gpu_accessed();
            return;
        }

        let time_val = ctx.time.seconds.0 as f32;
        let dt_scaled = ctx.time.delta.0 as f32 * 60.0;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident path; two derived fields). The bespoke simplex is inlined
            // in the body. apply_radial_burst_to_particles.wgsl (the hand-kernel parity oracle) was deleted 2026-07-20 (W1-B, migration scaffolding retired).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.add_burst standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.add_burst",
            )
        });

        let uniforms = BurstUniforms {
            point_x,
            point_y,
            amplitude,
            envelope,
            radius,
            active_count: active_count as i32,
            time_val,
            dt_scaled,
            dispatch_count: active_count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
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
            "node.add_burst",
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
            ApplyRadialBurstToParticles::TYPE_ID,
            "node.add_burst"
        );
        let names: Vec<&str> = ApplyRadialBurstToParticles::INPUTS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(
            names,
            vec!["in", "point_x", "point_y", "amplitude", "envelope", "radius", "active_count"]
        );
        assert_eq!(
            ApplyRadialBurstToParticles::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert!(ApplyRadialBurstToParticles::INPUTS[0].required);
        for input in &ApplyRadialBurstToParticles::INPUTS[1..] {
            assert!(!input.required, "{} should be optional", input.name);
        }

        assert_eq!(ApplyRadialBurstToParticles::OUTPUTS.len(), 1);
        assert_eq!(
            ApplyRadialBurstToParticles::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );

        let prim = ApplyRadialBurstToParticles::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ApplyRadialBurstToParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(
            node.type_id().as_str(),
            "node.add_burst"
        );
    }
}

