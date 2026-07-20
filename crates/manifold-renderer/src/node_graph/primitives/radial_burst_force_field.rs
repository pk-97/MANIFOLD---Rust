//! `node.explosion_force` — per-pixel vec2 force texture for
//! an impulse burst around a point.
//!
//! Produces a force field with:
//!   - Radial outward push from `(point_x, point_y)` over `radius`.
//!   - Tangent curl component (perpendicular to radial) for swirling
//!     impulses rather than pure point-explosions.
//!   - Noise-perturbed radial direction so the impulse looks organic
//!     rather than perfectly radial.
//!   - Falloff envelope `(1 - t²)²` where `t = dist / radius`.
//!   - Multiplied by `amplitude * envelope`.
//!
//! Composes into any flow-field particle pipeline by summing this
//! texture into the upstream velocity field (typically via
//! `node.mix(mode=Add)`). The downstream integrator picks up the
//! impulse on the next step. Reusable for any "impulse around a
//! point" use case — snaps, beat shoves, audio splashes, future
//! fluid sims.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BurstUniforms {
    point_x: f32,
    point_y: f32,
    amplitude: f32,
    envelope: f32,
    radius: f32,
    time_val: f32,
    _pad0: f32,
    _pad1: f32,
}

/// `noise_common.wgsl` prepended at pipeline creation so the noise
/// perturbation matches the rest of the renderer's noise math
/// bit-exactly (same Ashima 3D simplex `simplex3d` source). Lets the
/// burst integrate cleanly with simplex-based velocity fields.
const NOISE_COMMON: &str = include_str!("../../generators/shaders/noise_common.wgsl");

crate::primitive! {
    name: RadialBurstForceField,
    type_id: "node.explosion_force",
    purpose: "Produces a per-pixel vec2 force texture for a radial impulse burst around (point_x, point_y) within `radius`. Combines radial outward push, tangent curl, noise-perturbed radial direction, and a `(1-t²)²` falloff envelope, multiplied by `amplitude * envelope`. Sum into a velocity field via node.mix(Add) and let the downstream particle integrator pick up the impulse. Reusable for any 'impulse around a point' — snaps, beat shoves, audio splashes, fluid clip-trigger inject.",
    inputs: {
        point_x: ScalarF32 optional,
        point_y: ScalarF32 optional,
        amplitude: ScalarF32 optional,
        envelope: ScalarF32 optional,
        radius: ScalarF32 optional,
        time: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
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
            name: Cow::Borrowed("time"),
            label: "Time",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "All six inputs are port-shadow-param. Typical wiring: `node.inject_burst` produces (active, phase, point_x, point_y); wire point_x/point_y straight in, derive envelope from `active * envelope_decay(phase)` (or compose attack/decay externally), wire amplitude from an outer-card slider. When amplitude * envelope ≈ 0 the kernel early-outs to a zero texture — cheap when idle. Bit-exact noise perturbation via `noise_common.wgsl`'s simplex3d (same as `node.simplex_noise_per_copy` / `node.simplex_field_2d`).",
    examples: [],
    picker: { label: "Explosion Force", category: Atom },
    summary: "Makes a force field that pushes outward from a point, the field you feed into a particle move to drive an explosion.",
    category: Particles2D,
    role: Source,
    aliases: ["explosion force", "radial burst force field", "radial burst", "blast", "force field"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/radial_burst_force_field_body.wgsl"),
    wgsl_includes: [NOISE_COMMON],
}

impl Primitive for RadialBurstForceField {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let point_x = ctx.scalar_or_param("point_x", 0.5);
        let point_y = ctx.scalar_or_param("point_y", 0.5);
        let amplitude = ctx.scalar_or_param("amplitude", 0.0);
        let envelope = ctx.scalar_or_param("envelope", 0.0);
        let radius = ctx.scalar_or_param("radius", 0.25);
        let time_val = ctx.scalar_or_param("time", 0.0);

        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (target.width, target.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (texture source
            // path; NOISE_COMMON prepended via wgsl_includes for simplex3d).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.explosion_force standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.explosion_force",
            )
        });

        let uniforms = BurstUniforms {
            point_x,
            point_y,
            amplitude,
            envelope,
            radius,
            time_val,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.explosion_force",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_six_port_shadow_inputs_and_texture_out() {
        use crate::node_graph::ports::PortType;

        assert_eq!(RadialBurstForceField::TYPE_ID, "node.explosion_force");
        let names: Vec<&str> = RadialBurstForceField::INPUTS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(
            names,
            vec!["point_x", "point_y", "amplitude", "envelope", "radius", "time"]
        );
        for input in RadialBurstForceField::INPUTS {
            assert!(!input.required, "{} should be optional", input.name);
        }
        assert_eq!(RadialBurstForceField::OUTPUTS.len(), 1);
        assert_eq!(RadialBurstForceField::OUTPUTS[0].name, "out");
        assert_eq!(RadialBurstForceField::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn every_input_port_shadows_a_param() {
        // Standard port-shadow convention: scalar input + same-named
        // ParamDef. Both must exist for all six knobs.
        let port_names: Vec<&str> = RadialBurstForceField::INPUTS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        let param_names: Vec<&str> = RadialBurstForceField::PARAMS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        for name in &port_names {
            assert!(
                param_names.contains(name),
                "{name} input has no matching param"
            );
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = RadialBurstForceField::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.explosion_force");
    }
}

