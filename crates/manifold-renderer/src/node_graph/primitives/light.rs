//! `node.light` — single light source for the 3D lighting pipeline.
//!
//! Emits one [`Light`] on the `out` port; downstream consumers — shading
//! atoms ([`lambert_directional`](super::lambert_directional),
//! [`blinn_specular`](super::blinn_specular), …) and
//! shadow-aware mesh renderers — take it as a single `light: Light`
//! input instead of scattered `light_x/y/z/intensity` scalars.
//!
//! Two modes via the `mode` enum, matching Blender / TouchDesigner /
//! Unity convention:
//!
//! - **Sun** — parallel rays from a directional source. `pos` anchors the
//!   shadow ortho frustum; `aim` defines what the sun illuminates; `range`
//!   is the ortho half-extent.
//! - **Point** — omnidirectional source at `pos`. `aim - pos` gives the
//!   shadow camera's forward direction (single-cubemap-face approximation
//!   for v1); `range` is the attenuation half-distance.
//!
//! Per the design audit, `range` is a unified param that means "how far
//! does this light reach" in both modes — sun's ortho half-extent and
//! point's attenuation half-distance share the same conceptual knob, so
//! no slider is dead-state in any mode.
//!
//! Colour is premultiplied with `intensity` at emission so downstream
//! shading reads `light.color.rgb` directly.
//!
//! CPU-only — no GPU dispatch.

use std::borrow::Cow;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::light::{Light, ShadowSoftness};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const LIGHT_MODES: &[&str] = &["Sun", "Point"];
const SHADOW_SOFTNESS_LABELS: &[&str] = &["Hard", "Soft", "VerySoft", "Contact"];

crate::primitive! {
    name: LightNode,
    type_id: "node.light",
    purpose: "Single light source for 3D lighting pipelines. Mode enum picks Sun (parallel rays, ortho shadow frustum) or Point (omnidirectional, perspective shadow frustum). Outputs a Light wire consumed by shading atoms (lambert_directional, blinn_specular, etc.) and shadow-aware mesh renderers (the PBR path lives inside node.render_mesh's material). All scalar params are port-shadow so the light can be animated by LFOs, MIDI, or other control sources. Colour is premultiplied with intensity at emission. Industry-standard Blender / TouchDesigner shape — one node per light, shadow-mapping is a property of the light not a separate pipeline stage. shadow_softness's Contact tier (REALTIME_3D_DESIGN §11 D12) trades the fixed PCF kernel for PCSS contact-hardening: shadows go sharp where the caster touches the receiver and soften with distance, driven by the port-shadowed light_size (world-units light diameter).",
    inputs: {
        pos_x: ScalarF32 optional,
        pos_y: ScalarF32 optional,
        pos_z: ScalarF32 optional,
        aim_x: ScalarF32 optional,
        aim_y: ScalarF32 optional,
        aim_z: ScalarF32 optional,
        color_r: ScalarF32 optional,
        color_g: ScalarF32 optional,
        color_b: ScalarF32 optional,
        intensity: ScalarF32 optional,
        range: ScalarF32 optional,
        cast_shadows: ScalarF32 optional,
        shadow_bias: ScalarF32 optional,
        light_size: ScalarF32 optional,
    },
    outputs: {
        out: Light,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0), // Sun
            range: Some((0.0, (LIGHT_MODES.len() - 1) as f32)),
            enum_values: LIGHT_MODES,
        },
        ParamDef {
            name: Cow::Borrowed("pos_x"),
            label: "Position X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("pos_y"),
            label: "Position Y",
            ty: ParamType::Float,
            default: ParamValue::Float(30.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("pos_z"),
            label: "Position Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("aim_x"),
            label: "Aim X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("aim_y"),
            label: "Aim Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("aim_z"),
            label: "Aim Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_r"),
            label: "Colour R",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_g"),
            label: "Colour G",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_b"),
            label: "Colour B",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("intensity"),
            label: "Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("range"),
            label: "Range",
            ty: ParamType::Float,
            default: ParamValue::Float(30.0),
            range: Some((0.01, 200.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("cast_shadows"),
            label: "Cast Shadows",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("shadow_softness"),
            label: "Shadow Softness",
            ty: ParamType::Enum,
            default: ParamValue::Enum(1), // Soft (5x5)
            range: Some((0.0, (SHADOW_SOFTNESS_LABELS.len() - 1) as f32)),
            enum_values: SHADOW_SOFTNESS_LABELS,
        },
        ParamDef {
            name: Cow::Borrowed("shadow_bias"),
            label: "Shadow Bias",
            ty: ParamType::Float,
            default: ParamValue::Float(0.003),
            range: Some((0.0, 0.1)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("shadow_resolution"),
            label: "Shadow Resolution",
            ty: ParamType::Int,
            default: ParamValue::Float(2048.0),
            range: Some((128.0, 8192.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("light_size"),
            label: "Light Size",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 20.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Outer-card sliders typically expose pos/aim in world units, color as 0..1 RGB, intensity as a multiplier. `range` means different things per mode (Sun: shadow ortho half-extent, Point: attenuation half-distance) but both are 'how far does this light reach' — pick a value that matches your scene scale. `cast_shadows` is a [0, 1] threshold (> 0.5 = on) so it can be modulated by an LFO or trigger; toggle off to skip the shadow render pass entirely. `shadow_softness` picks the PCF kernel (3x3 / 5x5 / 7x7), or Contact for PCSS contact-hardening (shadows sharpen where the caster touches the receiver, soften with distance) — bigger fixed kernels and Contact both cost more than Hard. `light_size` only matters in Contact mode: it scales the blocker search and penumbra width, so it's the fader that turns noon (hard) into overcast (soft) on a self-shadowing hero mesh. `shadow_resolution` rarely needs perform-time control; bump it for sharper shadows on large scenes, drop it for performance. Wire `out` into a shadow-aware mesh renderer (renderer handles shadow map generation internally) or into a shading atom's `light` input (replaces scattered light_x/y/z scalars).",
    examples: [],
    picker: { label: "Light", category: Driver },
    summary: "A single light source for 3D scenes, set to a sun for parallel rays or a point for a local glow. Wire it into a material or a mesh renderer.",
    category: MaterialsAndLighting,
    role: Source,
    aliases: ["light", "lamp", "sun", "point light"],
    boundary_reason: NonGpu,
}

impl Primitive for LightNode {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let mode_idx = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => (*v).min((LIGHT_MODES.len() - 1) as u32),
            Some(ParamValue::Float(f)) => {
                f.round().clamp(0.0, (LIGHT_MODES.len() - 1) as f32) as u32
            }
            _ => 0,
        };

        let pos_x = ctx.scalar_or_param("pos_x", 0.0);
        let pos_y = ctx.scalar_or_param("pos_y", 30.0);
        let pos_z = ctx.scalar_or_param("pos_z", 0.0);
        let aim_x = ctx.scalar_or_param("aim_x", 0.0);
        let aim_y = ctx.scalar_or_param("aim_y", 0.0);
        let aim_z = ctx.scalar_or_param("aim_z", 0.0);
        let color_r = ctx.scalar_or_param("color_r", 1.0);
        let color_g = ctx.scalar_or_param("color_g", 1.0);
        let color_b = ctx.scalar_or_param("color_b", 1.0);
        let intensity = ctx.scalar_or_param("intensity", 1.0);
        let range = ctx.scalar_or_param("range", 30.0);
        let cast_shadows_f = ctx.scalar_or_param("cast_shadows", 1.0);
        let cast_shadows = cast_shadows_f > 0.5;
        let shadow_bias = ctx.scalar_or_param("shadow_bias", 0.003);

        let softness_idx = match ctx.params.get("shadow_softness") {
            Some(ParamValue::Enum(v)) => {
                (*v).min((SHADOW_SOFTNESS_LABELS.len() - 1) as u32)
            }
            Some(ParamValue::Float(f)) => f
                .round()
                .clamp(0.0, (SHADOW_SOFTNESS_LABELS.len() - 1) as f32)
                as u32,
            _ => 1,
        };
        let shadow_softness = match softness_idx {
            0 => ShadowSoftness::Hard,
            1 => ShadowSoftness::Soft,
            2 => ShadowSoftness::VerySoft,
            _ => ShadowSoftness::Contact {
                light_size: ctx.scalar_or_param("light_size", 1.0),
            },
        };

        let shadow_resolution = match ctx.params.get("shadow_resolution") {
            Some(ParamValue::Float(f)) => f.round().clamp(8.0, 16384.0) as u32,
            Some(ParamValue::Enum(v)) => (*v).max(8),
            _ => 2048,
        };

        let pos = [pos_x, pos_y, pos_z];
        let aim = [aim_x, aim_y, aim_z];
        let color_rgb = [color_r, color_g, color_b];

        let light = match mode_idx {
            1 => Light::point(
                pos,
                aim,
                color_rgb,
                intensity,
                range,
                cast_shadows,
                shadow_softness,
                shadow_bias,
                shadow_resolution,
            ),
            _ => Light::sun(
                pos,
                aim,
                color_rgb,
                intensity,
                range,
                cast_shadows,
                shadow_softness,
                shadow_bias,
                shadow_resolution,
            ),
        };

        ctx.outputs.set_light("out", light);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::light::LightMode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn light_declares_port_shadow_scalars_and_light_output() {
        use crate::node_graph::ports::{PortType, ScalarType};

        assert_eq!(LightNode::TYPE_ID, "node.light");
        for input in LightNode::INPUTS {
            assert!(!input.required, "{} should be optional (port-shadow)", input.name);
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(LightNode::OUTPUTS.len(), 1);
        assert_eq!(LightNode::OUTPUTS[0].name, "out");
        assert_eq!(LightNode::OUTPUTS[0].ty, PortType::Light);
    }

    #[test]
    fn light_has_mode_and_softness_enums_plus_full_scalar_surface() {
        let names: Vec<&str> = LightNode::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        for required in &[
            "mode",
            "pos_x",
            "pos_y",
            "pos_z",
            "aim_x",
            "aim_y",
            "aim_z",
            "color_r",
            "color_g",
            "color_b",
            "intensity",
            "range",
            "cast_shadows",
            "shadow_softness",
            "shadow_bias",
            "shadow_resolution",
            "light_size",
        ] {
            assert!(names.contains(required), "missing param {}", required);
        }
        let mode = LightNode::PARAMS.iter().find(|p| p.name == "mode").unwrap();
        assert_eq!(mode.ty, ParamType::Enum);
        let softness = LightNode::PARAMS
            .iter()
            .find(|p| p.name == "shadow_softness")
            .unwrap();
        assert_eq!(softness.ty, ParamType::Enum);
        let resolution = LightNode::PARAMS
            .iter()
            .find(|p| p.name == "shadow_resolution")
            .unwrap();
        assert_eq!(resolution.ty, ParamType::Int);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = LightNode::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.light");
    }

    #[test]
    fn run_emits_sun_light_by_default_with_premultiplied_color() {
        use crate::node_graph::MockBackend;
        use crate::node_graph::backend::Backend;
        use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
        use crate::node_graph::effect_node::ParamValues;
        use crate::node_graph::execution_plan::ResourceId;
        use crate::node_graph::ports::PortType;
        use manifold_core::{Beats, Seconds};

        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::Light, None, (0, 0));

        let mut params = ParamValues::default();
        // Default-driven: mode = Sun (0), intensity = 1.0, colour white.
        params.insert(std::borrow::Cow::Borrowed("mode"), ParamValue::Enum(0));
        params.insert(std::borrow::Cow::Borrowed("pos_x"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("pos_y"), ParamValue::Float(10.0));
        params.insert(std::borrow::Cow::Borrowed("pos_z"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("aim_x"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("aim_y"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("aim_z"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("color_r"), ParamValue::Float(0.5));
        params.insert(std::borrow::Cow::Borrowed("color_g"), ParamValue::Float(0.4));
        params.insert(std::borrow::Cow::Borrowed("color_b"), ParamValue::Float(0.3));
        params.insert(std::borrow::Cow::Borrowed("intensity"), ParamValue::Float(2.0));
        params.insert(std::borrow::Cow::Borrowed("range"), ParamValue::Float(20.0));
        params.insert(std::borrow::Cow::Borrowed("cast_shadows"), ParamValue::Float(1.0));
        params.insert(std::borrow::Cow::Borrowed("shadow_softness"), ParamValue::Enum(1));
        params.insert(std::borrow::Cow::Borrowed("shadow_bias"), ParamValue::Float(0.005));
        params.insert(std::borrow::Cow::Borrowed("shadow_resolution"), ParamValue::Float(1024.0));

        let mut prim = LightNode::new();
        let inputs_bindings: &[(&'static str, Slot)] = &[];
        let outputs_bindings: &[(&'static str, Slot)] = &[("out", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let inputs = NodeInputs::new(inputs_bindings, &backend, &[]);
        let outputs = NodeOutputs::new(
            outputs_bindings,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
        );
        let time = crate::node_graph::effect_node::FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        Primitive::run(&mut prim, &mut ctx);

        // Drain the queued light write into the backend, mirroring the
        // executor's per-step drain.
        for (slot, value) in light_scratch.drain(..) {
            backend.set_light(slot, value);
        }

        let light = backend.light(out_slot).expect("light should be set");
        assert_eq!(light.mode, LightMode::Sun);
        assert!(light.cast_shadows);
        // Colour premultiplied with intensity=2.0.
        assert!((light.color[0] - 1.0).abs() < 1e-5);
        assert!((light.color[1] - 0.8).abs() < 1e-5);
        assert!((light.color[2] - 0.6).abs() < 1e-5);
        assert_eq!(light.shadow_bias, 0.005);
        assert_eq!(light.shadow_resolution, 1024);
        assert_eq!(light.range, 20.0);
        assert_eq!(light.shadow_softness, ShadowSoftness::Soft);
    }

    #[test]
    fn run_with_mode_point_emits_point_light() {
        use crate::node_graph::MockBackend;
        use crate::node_graph::backend::Backend;
        use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
        use crate::node_graph::effect_node::ParamValues;
        use crate::node_graph::execution_plan::ResourceId;
        use crate::node_graph::ports::PortType;
        use manifold_core::{Beats, Seconds};

        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::Light, None, (0, 0));
        let mut params = ParamValues::default();
        params.insert(std::borrow::Cow::Borrowed("mode"), ParamValue::Enum(1)); // Point
        params.insert(std::borrow::Cow::Borrowed("pos_x"), ParamValue::Float(1.0));
        params.insert(std::borrow::Cow::Borrowed("pos_y"), ParamValue::Float(2.0));
        params.insert(std::borrow::Cow::Borrowed("pos_z"), ParamValue::Float(3.0));
        params.insert(std::borrow::Cow::Borrowed("aim_x"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("aim_y"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("aim_z"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("color_r"), ParamValue::Float(1.0));
        params.insert(std::borrow::Cow::Borrowed("color_g"), ParamValue::Float(1.0));
        params.insert(std::borrow::Cow::Borrowed("color_b"), ParamValue::Float(1.0));
        params.insert(std::borrow::Cow::Borrowed("intensity"), ParamValue::Float(1.0));
        params.insert(std::borrow::Cow::Borrowed("range"), ParamValue::Float(25.0));
        params.insert(std::borrow::Cow::Borrowed("cast_shadows"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("shadow_softness"), ParamValue::Enum(2)); // VerySoft
        params.insert(std::borrow::Cow::Borrowed("shadow_bias"), ParamValue::Float(0.003));
        params.insert(std::borrow::Cow::Borrowed("shadow_resolution"), ParamValue::Float(2048.0));

        let mut prim = LightNode::new();
        let inputs_bindings: &[(&'static str, Slot)] = &[];
        let outputs_bindings: &[(&'static str, Slot)] = &[("out", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let inputs = NodeInputs::new(inputs_bindings, &backend, &[]);
        let outputs = NodeOutputs::new(
            outputs_bindings,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
        );
        let time = crate::node_graph::effect_node::FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        Primitive::run(&mut prim, &mut ctx);

        for (slot, value) in light_scratch.drain(..) {
            backend.set_light(slot, value);
        }

        let light = backend.light(out_slot).expect("light should be set");
        assert_eq!(light.mode, LightMode::Point);
        assert!(!light.cast_shadows);
        assert_eq!(light.shadow_softness, ShadowSoftness::VerySoft);
        // Attenuation at distance = range should be ~0.5.
        // Light is at (1, 2, 3); test point at distance 25 along +x.
        let test_pt = [1.0 + 25.0, 2.0, 3.0];
        let att = light.attenuation_at(test_pt);
        assert!((att - 0.5).abs() < 1e-4);
    }

    #[test]
    fn run_with_contact_shadow_softness_emits_light_size() {
        use crate::node_graph::MockBackend;
        use crate::node_graph::backend::Backend;
        use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
        use crate::node_graph::effect_node::ParamValues;
        use crate::node_graph::execution_plan::ResourceId;
        use crate::node_graph::ports::PortType;
        use manifold_core::{Beats, Seconds};

        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::Light, None, (0, 0));
        let mut params = ParamValues::default();
        params.insert(std::borrow::Cow::Borrowed("mode"), ParamValue::Enum(0));
        params.insert(std::borrow::Cow::Borrowed("pos_x"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("pos_y"), ParamValue::Float(10.0));
        params.insert(std::borrow::Cow::Borrowed("pos_z"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("aim_x"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("aim_y"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("aim_z"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("color_r"), ParamValue::Float(1.0));
        params.insert(std::borrow::Cow::Borrowed("color_g"), ParamValue::Float(1.0));
        params.insert(std::borrow::Cow::Borrowed("color_b"), ParamValue::Float(1.0));
        params.insert(std::borrow::Cow::Borrowed("intensity"), ParamValue::Float(1.0));
        params.insert(std::borrow::Cow::Borrowed("range"), ParamValue::Float(20.0));
        params.insert(std::borrow::Cow::Borrowed("cast_shadows"), ParamValue::Float(1.0));
        params.insert(std::borrow::Cow::Borrowed("shadow_softness"), ParamValue::Enum(3)); // Contact
        params.insert(std::borrow::Cow::Borrowed("shadow_bias"), ParamValue::Float(0.003));
        params.insert(std::borrow::Cow::Borrowed("shadow_resolution"), ParamValue::Float(2048.0));
        params.insert(std::borrow::Cow::Borrowed("light_size"), ParamValue::Float(2.5));

        let mut prim = LightNode::new();
        let inputs_bindings: &[(&'static str, Slot)] = &[];
        let outputs_bindings: &[(&'static str, Slot)] = &[("out", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let inputs = NodeInputs::new(inputs_bindings, &backend, &[]);
        let outputs = NodeOutputs::new(
            outputs_bindings,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
        );
        let time = crate::node_graph::effect_node::FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        Primitive::run(&mut prim, &mut ctx);

        for (slot, value) in light_scratch.drain(..) {
            backend.set_light(slot, value);
        }

        let light = backend.light(out_slot).expect("light should be set");
        assert_eq!(light.shadow_softness, ShadowSoftness::Contact { light_size: 2.5 });
    }
}
