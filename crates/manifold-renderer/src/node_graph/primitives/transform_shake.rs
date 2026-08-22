//! `node.transform_shake` — stateless shake/jitter on a `Transform` wire.
//!
//! Pure CPU pass-through (no GPU dispatch): takes a `Transform` in, adds a
//! stateless summed-sine noise offset, and emits a `Transform` out. The
//! noise is a function of `time × frequency` only — no stored phase, no
//! RNG state. Rotational jitter is dominant; positional jitter is the same
//! noise vector at a fixed 0.25 ratio. The response curve is `amount²`, so
//! the default amount of 0 is a byte-identical passthrough and small values
//! ramp smoothly from silence.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::transform::Transform;

/// Phase offsets per axis so each Euler component shakes with different
/// timing while staying derived from the same time value.
const AXIS_PHASES: [f32; 3] = [0.0, 1.2345678, 2.3456789];

/// Irrational-ish frequency ratios for the summed-sine noise. The absolute
/// scale is folded into the `frequency` param; these ratios keep the three
/// sines from lining up into obvious periodicity.
const NOISE_FREQS: [f32; 4] = [1.0, 2.7182817, 4.6692016, 7.389056];

/// Normalisation so the sum sits roughly in [-1, 1].
const NOISE_SCALE: f32 = 0.25;

crate::primitive! {
    name: TransformShake,
    type_id: "node.transform_shake",
    purpose: "Stateless camera/object shake on a Transform wire. Adds a summed-sine noise offset (rotational dominant, positional at 0.25 ratio) driven by time × frequency. amount² response means amount = 0 is a byte-identical passthrough.",
    inputs: {
        transform: Transform required,
        amount: ScalarF32 optional,
        frequency: ScalarF32 optional,
        time: ScalarF32 optional,
    },
    outputs: {
        out: Transform,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("frequency"),
            label: "Frequency",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("time"),
            label: "Time",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Insert between node.transform_3d and node.scene_object (or node.render_scene's transform_n) to add physical weight — a kick drum on amount, a subtle idle shimmer at low values. Drive time from a beat ramp or leave it unwired for continuous playback-clock animation. No state: reconnecting the wire or reloading the project produces identical output for the same time.",
    examples: [],
    picker: { label: "Shake", category: Atom },
    summary: "Stateless shake on a Transform wire — rotational jitter dominant, positional at a quarter ratio, driven by time and frequency.",
    category: Geometry3D,
    role: Filter,
    aliases: ["shake", "jitter", "camera shake", "transform shake", "vibrate"],
    boundary_reason: NonGpu,
}

impl Primitive for TransformShake {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(input) = ctx.inputs.transform("transform") else {
            return;
        };

        let amount = ctx.scalar_or_param("amount", 0.0);
        let frequency = ctx.scalar_or_param("frequency", 1.0);
        let time = ctx.scalar_or_param("time", 0.0);

        let scaled_time = time * frequency;
        let mut noise = [0.0f32; 3];
        for i in 0..3 {
            let mut sum = 0.0f32;
            for &f in &NOISE_FREQS {
                sum += (scaled_time * f + AXIS_PHASES[i]).sin();
            }
            noise[i] = sum * NOISE_SCALE;
        }

        let amount_sq = amount * amount;
        let rot_offset = [noise[0] * amount_sq, noise[1] * amount_sq, noise[2] * amount_sq];
        let pos_offset = [
            rot_offset[0] * 0.25,
            rot_offset[1] * 0.25,
            rot_offset[2] * 0.25,
        ];

        ctx.outputs.set_transform(
            "out",
            Transform {
                pos: [
                    input.pos[0] + pos_offset[0],
                    input.pos[1] + pos_offset[1],
                    input.pos[2] + pos_offset[2],
                ],
                rot_euler: [
                    input.rot_euler[0] + rot_offset[0],
                    input.rot_euler[1] + rot_offset[1],
                    input.rot_euler[2] + rot_offset[2],
                ],
                scale: input.scale,
            },
        );
    }
}

/// Helper so tests can compute the expected noise vector for a given
/// `time × frequency` value. Mirrors the implementation exactly.
#[cfg(test)]
fn shake_noise_vector(scaled_time: f32) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for i in 0..3 {
        let mut sum = 0.0f32;
        for &f in &NOISE_FREQS {
            sum += (scaled_time * f + AXIS_PHASES[i]).sin();
        }
        out[i] = sum * NOISE_SCALE;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::MockBackend;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
    use crate::node_graph::effect_node::{FrameTime, ParamValues};
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::ports::PortType;
    use manifold_core::{Beats, Seconds};

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn run_with_params_and_wires(
        input: Transform,
        overrides: &[(&'static str, f32)],
        wires: &[(&'static str, f32)],
    ) -> Transform {
        let defaults: &[(&str, f32)] = &[
            ("amount", 0.0),
            ("frequency", 1.0),
            ("time", 0.0),
        ];

        let mut backend = MockBackend::new();
        let in_slot = backend.acquire(ResourceId(0), PortType::Transform, None, (0, 0));
        backend.set_transform(in_slot, input);
        let out_slot = backend.acquire(ResourceId(1), PortType::Transform, None, (0, 0));

        let mut params = ParamValues::default();
        for &(name, default) in defaults {
            let value = overrides
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| *v)
                .unwrap_or(default);
            params.insert(Cow::Owned(name.to_string()), ParamValue::Float(value));
        }

        let mut wire_slots: Vec<(&'static str, Slot)> = vec![("transform", in_slot)];
        let mut next_id = 2u32;
        for &(name, value) in wires {
            let slot = backend.acquire(
                ResourceId(next_id),
                crate::node_graph::ports::PortType::Scalar(crate::node_graph::ports::ScalarType::F32),
                None,
                (0, 0),
            );
            next_id += 1;
            backend.set_scalar(slot, ParamValue::Float(value));
            wire_slots.push((name, slot));
        }

        let mut prim = TransformShake::new();
        let outputs_bindings: &[(&'static str, Slot)] = &[("out", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let inputs = NodeInputs::new(&wire_slots, &backend, &[]);
        let outputs = NodeOutputs::new(
            outputs_bindings,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
            &mut object_scratch,
        );
        let time = frame_time();
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        Primitive::run(&mut prim, &mut ctx);

        for (slot, value) in transform_scratch.drain(..) {
            backend.set_transform(slot, value);
        }

        backend.transform(out_slot).expect("transform should be set")
    }

    #[test]
    fn declares_transform_in_out_and_three_port_shadow_scalars() {
        assert_eq!(TransformShake::TYPE_ID, "node.transform_shake");
        let in_names: Vec<&str> = TransformShake::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(in_names, vec!["transform", "amount", "frequency", "time"]);
        assert_eq!(TransformShake::INPUTS[0].ty, PortType::Transform);
        assert!(TransformShake::INPUTS[0].required);
        for input in &TransformShake::INPUTS[1..] {
            assert!(!input.required, "{} should be optional (port-shadow)", input.name);
            assert_eq!(input.ty, PortType::Scalar(crate::node_graph::ports::ScalarType::F32));
        }
        assert_eq!(TransformShake::OUTPUTS.len(), 1);
        assert_eq!(TransformShake::OUTPUTS[0].name, "out");
        assert_eq!(TransformShake::OUTPUTS[0].ty, PortType::Transform);
    }

    #[test]
    fn has_three_params() {
        let names: Vec<&str> = TransformShake::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["amount", "frequency", "time"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = TransformShake::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.transform_shake");
    }

    #[test]
    fn amount_zero_is_byte_identical_passthrough() {
        let input = Transform {
            pos: [1.0, -2.5, 3.75],
            rot_euler: [0.1, -0.2, 0.3],
            scale: [1.0, 2.0, 0.5],
        };
        let out = run_with_params_and_wires(input, &[("amount", 0.0)], &[]);
        assert_eq!(out, input, "amount = 0 must pass the transform through unchanged");
    }

    #[test]
    fn value_matches_hand_computed_noise() {
        let input = Transform {
            pos: [0.0; 3],
            rot_euler: [0.0; 3],
            scale: [1.0; 3],
        };
        let amount = 2.0f32;
        let frequency = 3.5f32;
        let time = 1.25f32;
        let out = run_with_params_and_wires(
            input,
            &[("amount", amount), ("frequency", frequency), ("time", time)],
            &[],
        );

        let noise = shake_noise_vector(time * frequency);
        let amount_sq = amount * amount;
        let expected_rot = [noise[0] * amount_sq, noise[1] * amount_sq, noise[2] * amount_sq];
        let expected_pos = [expected_rot[0] * 0.25, expected_rot[1] * 0.25, expected_rot[2] * 0.25];

        assert_eq!(out.rot_euler, expected_rot, "rotational offset must match hand formula");
        assert_eq!(out.pos, expected_pos, "positional offset must be 0.25 of rotational");
        assert_eq!(out.scale, input.scale, "scale must pass through unchanged");
    }

    #[test]
    fn wired_amount_overrides_its_same_named_param() {
        let input = Transform::default();
        // Param says 2.0; the wire says 0.0 — output must be passthrough.
        let out = run_with_params_and_wires(input, &[("amount", 2.0)], &[("amount", 0.0)]);
        assert_eq!(out, input, "wired amount should override the param");
    }

    #[test]
    fn wired_time_overrides_playback_clock() {
        let input = Transform::default();
        let out = run_with_params_and_wires(
            input,
            &[("amount", 1.0), ("frequency", 1.0), ("time", 0.5)],
            &[("time", 2.0)],
        );
        // With time wired to 2.0, expected noise uses scaled_time = 2.0.
        let noise = shake_noise_vector(2.0);
        assert_eq!(out.rot_euler[0], noise[0], "wired time should drive the noise");
    }
}
