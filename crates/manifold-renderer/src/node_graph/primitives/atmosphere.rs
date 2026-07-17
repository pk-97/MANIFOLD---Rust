//! `node.atmosphere` — scene-wide fog + sky-tint producer.
//!
//! Per `docs/REALTIME_3D_DESIGN.md` D5 / §5 P3: emits a single
//! [`Atmosphere`] struct (exponential depth fog colour + density + height
//! falloff + ambient tint), consumed by `render_scene`'s optional
//! `atmosphere` input. Every param is port-shadowed by a same-named optional
//! scalar input (prefer `ctx.inputs.scalar`, fall back to the param) so fog
//! density on a fader or a `beat_ramp` is a live depth-mood knob — the sizzle
//! D5 exists for.
//!
//! CPU-only — no GPU dispatch. The wire carries plain scalars; the fog maths
//! (per-fragment `1 - exp(-density·distance)`) run inside `render_scene`'s lit
//! shaders. Unwired into `render_scene` = [`Atmosphere::default`] = fog off =
//! byte-identical to no atmosphere.

use std::borrow::Cow;

use crate::node_graph::atmosphere::Atmosphere;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// `shaft_quality` enum labels, index = `Atmosphere::shaft_quality`
/// (VOLUMETRIC_LIGHT_DESIGN.md D1: `0` Low/16 steps, `1` Med/24 (default),
/// `2` High/32).
const SHAFT_QUALITIES: &[&str] = &["Low", "Med", "High"];

crate::primitive! {
    name: AtmosphereNode,
    type_id: "node.atmosphere",
    purpose: "Scene-wide atmosphere producer: exponential depth fog (colour + density + height falloff) plus an ambient/sky tint, emitted as a single Atmosphere struct consumed by render_scene's optional `atmosphere` input. Fog fades distant geometry toward fog_color by 1 - exp(-density·distance); height_falloff concentrates it near the ground (y=0) for a haze look; ambient_tint multiplies each object's ambient term. Every param is port-shadowed by a same-named optional scalar input, so fog density on a fader or beat_ramp is a live depth-mood knob. Unwired into render_scene = fog off (density 0), byte-identical to no atmosphere.",
    inputs: {
        fog_color_r: ScalarF32 optional,
        fog_color_g: ScalarF32 optional,
        fog_color_b: ScalarF32 optional,
        fog_density: ScalarF32 optional,
        height_falloff: ScalarF32 optional,
        ambient_tint_r: ScalarF32 optional,
        ambient_tint_g: ScalarF32 optional,
        ambient_tint_b: ScalarF32 optional,
        shaft_intensity: ScalarF32 optional,
        shaft_anisotropy: ScalarF32 optional,
    },
    outputs: {
        atmosphere: Atmosphere,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("fog_color_r"),
            label: "Fog Color R",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("fog_color_g"),
            label: "Fog Color G",
            ty: ParamType::Float,
            default: ParamValue::Float(0.55),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("fog_color_b"),
            label: "Fog Color B",
            ty: ParamType::Float,
            default: ParamValue::Float(0.65),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("fog_density"),
            label: "Fog Density",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("height_falloff"),
            label: "Height Falloff",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("ambient_tint_r"),
            label: "Ambient Tint R",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("ambient_tint_g"),
            label: "Ambient Tint G",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("ambient_tint_b"),
            label: "Ambient Tint B",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("shaft_intensity"),
            label: "Shaft Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("shaft_anisotropy"),
            label: "Shaft Anisotropy",
            ty: ParamType::Float,
            default: ParamValue::Float(0.6),
            range: Some((-0.9, 0.9)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("shaft_quality"),
            label: "Shaft Quality",
            ty: ParamType::Enum,
            default: ParamValue::Enum(1),
            range: Some((0.0, (SHAFT_QUALITIES.len() - 1) as f32)),
            enum_values: SHAFT_QUALITIES,
        },
    ],
    composition_notes: "Wire `atmosphere` into render_scene's `atmosphere` input (unwired = fog off, byte-identical to no atmosphere). fog_density is per-world-unit — ~0.05 is a light haze over tens of units, ~0.3 is thick. height_falloff > 0 concentrates fog near y=0 (ground haze). Each param is independently port-shadowed: wire fog_density to a macro/LFO for a live depth-mood fader, leave the rest static.",
    examples: [],
    picker: { label: "Atmosphere", category: Driver },
    summary: "Scene fog + sky tint for render_scene. Wire it into a scene's atmosphere input; put fog density on a fader for an instant depth-mood knob.",
    category: Geometry3D,
    role: Source,
    aliases: ["atmosphere", "fog", "haze", "depth fog", "mist", "sky tint"],
    boundary_reason: NonGpu,
}

impl Primitive for AtmosphereNode {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let fog_color = [
            ctx.scalar_or_param("fog_color_r", 0.5),
            ctx.scalar_or_param("fog_color_g", 0.55),
            ctx.scalar_or_param("fog_color_b", 0.65),
            1.0,
        ];
        let ambient_tint = [
            ctx.scalar_or_param("ambient_tint_r", 1.0),
            ctx.scalar_or_param("ambient_tint_g", 1.0),
            ctx.scalar_or_param("ambient_tint_b", 1.0),
            1.0,
        ];
        let max_quality = (SHAFT_QUALITIES.len() - 1) as u32;
        let shaft_quality = match ctx.params.get("shaft_quality") {
            Some(ParamValue::Enum(v)) => (*v).min(max_quality),
            Some(ParamValue::Float(f)) => (f.round().max(0.0) as u32).min(max_quality),
            _ => 1,
        };
        let atmosphere = Atmosphere {
            fog_color,
            fog_density: ctx.scalar_or_param("fog_density", 0.0).max(0.0),
            height_falloff: ctx.scalar_or_param("height_falloff", 0.0).max(0.0),
            ambient_tint,
            shaft_intensity: ctx.scalar_or_param("shaft_intensity", 0.0).max(0.0),
            shaft_anisotropy: ctx.scalar_or_param("shaft_anisotropy", 0.6).clamp(-0.9, 0.9),
            shaft_quality,
        };
        ctx.outputs.set_atmosphere("atmosphere", atmosphere);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::MockBackend;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
    use crate::node_graph::effect_node::{FrameTime, ParamValues};
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::ports::PortType;
    use crate::node_graph::primitive::PrimitiveSpec;
    use manifold_core::{Beats, Seconds};

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    #[test]
    fn declares_ten_port_shadow_scalars_and_atmosphere_output() {
        assert_eq!(AtmosphereNode::TYPE_ID, "node.atmosphere");
        assert_eq!(AtmosphereNode::INPUTS.len(), 10, "8 original + shaft_intensity + shaft_anisotropy (shaft_quality is param-only, not port-shadowed)");
        for input in AtmosphereNode::INPUTS {
            assert!(!input.required, "{} should be optional (port-shadow)", input.name);
        }
        assert_eq!(AtmosphereNode::OUTPUTS.len(), 1);
        assert_eq!(AtmosphereNode::OUTPUTS[0].name, "atmosphere");
        assert_eq!(AtmosphereNode::OUTPUTS[0].ty, PortType::Atmosphere);
    }

    #[test]
    fn registers_as_palette_atom() {
        let prim = AtmosphereNode::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.atmosphere");
    }

    /// Run `AtmosphereNode` with the given param overrides (defaults for the
    /// rest), no wired scalars, returning the emitted `Atmosphere`.
    fn run_with_params(overrides: &[(&'static str, f32)]) -> Atmosphere {
        let defaults: &[(&str, f32)] = &[
            ("fog_color_r", 0.5),
            ("fog_color_g", 0.55),
            ("fog_color_b", 0.65),
            ("fog_density", 0.0),
            ("height_falloff", 0.0),
            ("ambient_tint_r", 1.0),
            ("ambient_tint_g", 1.0),
            ("ambient_tint_b", 1.0),
            ("shaft_intensity", 0.0),
            ("shaft_anisotropy", 0.6),
        ];

        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::Atmosphere, None, (0, 0));

        let mut params = ParamValues::default();
        for &(name, default) in defaults {
            let value = overrides
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| *v)
                .unwrap_or(default);
            params.insert(Cow::Owned(name.to_string()), ParamValue::Float(value));
        }

        let mut prim = AtmosphereNode::new();
        let outputs_bindings: &[(&'static str, Slot)] = &[("atmosphere", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let wire_slots: Vec<(&'static str, Slot)> = Vec::new();
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

        for (slot, value) in atmosphere_scratch.drain(..) {
            backend.set_atmosphere(slot, value);
        }
        backend.atmosphere(out_slot).expect("atmosphere should be set")
    }

    #[test]
    fn unwired_defaults_produce_fog_off_atmosphere() {
        let a = run_with_params(&[]);
        assert_eq!(a.fog_density, 0.0, "default must be fog off");
        assert_eq!(a, Atmosphere::default());
    }

    #[test]
    fn params_flow_through_to_atmosphere_fields() {
        let a = run_with_params(&[
            ("fog_color_r", 0.1),
            ("fog_color_g", 0.2),
            ("fog_color_b", 0.3),
            ("fog_density", 0.25),
            ("height_falloff", 0.5),
            ("ambient_tint_r", 1.5),
        ]);
        assert_eq!(a.fog_color, [0.1, 0.2, 0.3, 1.0]);
        assert_eq!(a.fog_density, 0.25);
        assert_eq!(a.height_falloff, 0.5);
        assert_eq!(a.ambient_tint, [1.5, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn negative_density_is_clamped_to_zero() {
        let a = run_with_params(&[("fog_density", -1.0)]);
        assert_eq!(a.fog_density, 0.0, "density must never go negative (exp would blow up)");
    }

    #[test]
    fn shaft_params_flow_through_to_atmosphere_fields() {
        let a = run_with_params(&[("shaft_intensity", 1.5), ("shaft_anisotropy", -0.3)]);
        assert_eq!(a.shaft_intensity, 1.5);
        assert_eq!(a.shaft_anisotropy, -0.3);
        assert_eq!(a.shaft_quality, 1, "no shaft_quality override -> default Med (1)");
    }

    #[test]
    fn negative_shaft_intensity_is_clamped_to_zero() {
        let a = run_with_params(&[("shaft_intensity", -2.0)]);
        assert_eq!(a.shaft_intensity, 0.0, "shaft_intensity must never go negative");
    }

    #[test]
    fn shaft_anisotropy_is_clamped_to_henyey_greenstein_range() {
        let hot = run_with_params(&[("shaft_anisotropy", 5.0)]);
        assert_eq!(hot.shaft_anisotropy, 0.9);
        let cold = run_with_params(&[("shaft_anisotropy", -5.0)]);
        assert_eq!(cold.shaft_anisotropy, -0.9);
    }

    #[test]
    fn shaft_quality_enum_reads_back_as_index() {
        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::Atmosphere, None, (0, 0));
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("shaft_quality"), ParamValue::Enum(2));
        let mut prim = AtmosphereNode::new();
        let outputs_bindings: &[(&'static str, Slot)] = &[("atmosphere", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let wire_slots: Vec<(&'static str, Slot)> = Vec::new();
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
        for (slot, value) in atmosphere_scratch.drain(..) {
            backend.set_atmosphere(slot, value);
        }
        let a = backend.atmosphere(out_slot).expect("atmosphere should be set");
        assert_eq!(a.shaft_quality, 2, "High (index 2) must read back exactly");
    }
}
