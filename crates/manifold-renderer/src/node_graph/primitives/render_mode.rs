//! `node.render_mode` — scene-wide viewport shading-mode producer.
//!
//! Per `docs/SCENE_RENDER_MODE_DESIGN.md` D3: emits a single
//! [`RenderMode`] struct (mode index + clay color + line color +
//! line brightness + point size), consumed by `render_scene`'s optional
//! `render_mode` input. Every param is port-shadowed by a same-named
//! optional scalar input (prefer the wired scalar, fall back to the param)
//! so the mode and every color/gain row is drivable from a fader, an LFO,
//! or a clip envelope — the performability the design exists for.
//!
//! CPU-only — no GPU dispatch. The wire carries plain scalars; the raster
//! consequences (fill mode, unlit line material) are applied inside
//! `render_scene`. Unwired into `render_scene` = [`RenderMode::default`]
//! = mode Rendered = byte-identical to no render_mode.
//!
//! D5's enable gate multiplies INTO the port-shadowed `mode` scalar
//! (`enabled × mode → Mul → mode input`), so on the wire `mode` is a
//! continuous float that the atom rounds + clamps into the index range —
//! the same shape as fog's density gate at partial enable.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::render_mode::{RENDER_MODE_LABELS, RenderMode};

crate::primitive! {
    name: RenderModeNode,
    type_id: "node.render_mode",
    purpose: "Scene-wide viewport shading-mode producer: Blender-style Rendered/Solid/Wireframe/Points as a performable scene modifier, emitted as a single RenderMode struct consumed by render_scene's optional `render_mode` input (SCENE_RENDER_MODE_DESIGN.md). Wireframe draws the color pass as triangle lines with an unlit line_color × line_brightness material; Solid substitutes a flat clay Phong material; Points draws the mesh as points. Every param is port-shadowed by a same-named optional scalar input, so the mode row on a MIDI pad or line brightness on the kick is a live look switch. Unwired into render_scene = Rendered = byte-identical to no render_mode.",
    inputs: {
        mode: ScalarF32 optional,
        clay_color_r: ScalarF32 optional,
        clay_color_g: ScalarF32 optional,
        clay_color_b: ScalarF32 optional,
        line_color_r: ScalarF32 optional,
        line_color_g: ScalarF32 optional,
        line_color_b: ScalarF32 optional,
        line_brightness: ScalarF32 optional,
        point_size: ScalarF32 optional,
    },
    outputs: {
        render_mode: RenderMode,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, (RENDER_MODE_LABELS.len() - 1) as f32)),
            enum_values: RENDER_MODE_LABELS,
        },
        ParamDef {
            name: Cow::Borrowed("clay_color_r"),
            label: "Clay Color R",
            ty: ParamType::Float,
            default: ParamValue::Float(0.8),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("clay_color_g"),
            label: "Clay Color G",
            ty: ParamType::Float,
            default: ParamValue::Float(0.8),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("clay_color_b"),
            label: "Clay Color B",
            ty: ParamType::Float,
            default: ParamValue::Float(0.8),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("line_color_r"),
            label: "Line Color R",
            ty: ParamType::Float,
            default: ParamValue::Float(0.1),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("line_color_g"),
            label: "Line Color G",
            ty: ParamType::Float,
            default: ParamValue::Float(0.9),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("line_color_b"),
            label: "Line Color B",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("line_brightness"),
            label: "Line Brightness",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("point_size"),
            label: "Point Size",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((1.0, 16.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire `render_mode` into render_scene's `render_mode` input (unwired = Rendered, byte-identical to no input). The enable gate multiplies INTO the port-shadowed `mode` scalar: enabled=0 → mode 0 → Rendered (INV-R2: Rendered is index 0 forever). mode is a continuous float on the wire inside the gate — the atom rounds + clamps it, so a modulated enabled at 0.5 floors the mode index, same behavior as fog's gate at partial enable. The per-mode floats are NOT gated: inert when their mode isn't active (clay_color unread outside Solid, line_color/line_brightness unread outside Wireframe/Points, point_size unread outside Points).",
    examples: [],
    picker: { label: "Render Mode", category: Driver },
    summary: "Viewport shading mode (Rendered/Solid/Wireframe/Points) for render_scene. Wire it into a scene's render_mode input; put the mode row on a MIDI pad for a live wireframe drop.",
    category: Geometry3D,
    role: Source,
    aliases: ["render mode", "wireframe", "solid", "points", "viewport shading", "shading mode", "clay"],
    boundary_reason: NonGpu,
}

impl Primitive for RenderModeNode {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // D5: the wired scalar wins when the enable gate is patched in;
        // otherwise the Enum param is the source. Either way the value is
        // a continuous float on the wire — round + clamp into the index
        // range (a modulated gate at 0.5 floors, same as fog).
        let mode_param = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => *v as f32,
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let max_mode = (RENDER_MODE_LABELS.len() - 1) as f32;
        let mode = ctx
            .scalar_or_param("mode", mode_param)
            .round()
            .clamp(0.0, max_mode) as u32;
        let render_mode = RenderMode {
            mode,
            clay_color: [
                ctx.scalar_or_param("clay_color_r", 0.8),
                ctx.scalar_or_param("clay_color_g", 0.8),
                ctx.scalar_or_param("clay_color_b", 0.8),
                1.0,
            ],
            line_color: [
                ctx.scalar_or_param("line_color_r", 0.1),
                ctx.scalar_or_param("line_color_g", 0.9),
                ctx.scalar_or_param("line_color_b", 1.0),
                1.0,
            ],
            line_brightness: ctx.scalar_or_param("line_brightness", 1.0).clamp(0.0, 4.0),
            point_size: ctx.scalar_or_param("point_size", 2.0).clamp(1.0, 16.0),
        };
        ctx.outputs.set_render_mode("render_mode", render_mode);
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
    fn declares_nine_port_shadow_scalars_and_render_mode_output() {
        assert_eq!(RenderModeNode::TYPE_ID, "node.render_mode");
        assert_eq!(RenderModeNode::INPUTS.len(), 9);
        for input in RenderModeNode::INPUTS {
            assert!(!input.required, "{} should be optional (port-shadow)", input.name);
        }
        assert_eq!(RenderModeNode::OUTPUTS.len(), 1);
        assert_eq!(RenderModeNode::OUTPUTS[0].name, "render_mode");
        assert_eq!(RenderModeNode::OUTPUTS[0].ty, PortType::RenderMode);
    }

    #[test]
    fn registers_as_palette_atom() {
        let prim = RenderModeNode::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.render_mode");
    }

    const DEFAULTS: &[(&str, f32)] = &[
        ("clay_color_r", 0.8),
        ("clay_color_g", 0.8),
        ("clay_color_b", 0.8),
        ("line_color_r", 0.1),
        ("line_color_g", 0.9),
        ("line_color_b", 1.0),
        ("line_brightness", 1.0),
        ("point_size", 2.0),
    ];

    /// Run `RenderModeNode` with the given float param overrides (defaults
    /// for the rest) and optional wired scalar overrides, returning the
    /// emitted `RenderMode`.
    fn run_with(overrides: &[(&'static str, f32)], wired: &[(&'static str, f32)]) -> RenderMode {
        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::RenderMode, None, (0, 0));

        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut render_mode_scratch = Vec::new();
        let mut object_scratch = Vec::new();

        let wire_slots: Vec<(&'static str, Slot)> = wired
            .iter()
            .enumerate()
            .map(|(i, (name, value))| {
                let slot = backend.acquire(
                    ResourceId(1 + i as u32),
                    PortType::Scalar(crate::node_graph::ports::ScalarType::F32),
                    None,
                    (0, 0),
                );
                backend.set_scalar(slot, ParamValue::Float(*value));
                (*name, slot)
            })
            .collect();

        let mut params = ParamValues::default();
        for &(name, default) in DEFAULTS {
            let value = overrides
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| *v)
                .unwrap_or(default);
            params.insert(Cow::Owned(name.to_string()), ParamValue::Float(value));
        }
        let mode_param = overrides
            .iter()
            .find(|(n, _)| *n == "mode")
            .map(|(_, v)| ParamValue::Float(*v))
            .unwrap_or(ParamValue::Enum(0));
        params.insert(Cow::Borrowed("mode"), mode_param);

        let mut prim = RenderModeNode::new();
        let outputs_bindings: &[(&'static str, Slot)] = &[("render_mode", out_slot)];
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
            &mut render_mode_scratch,
            &mut object_scratch,
        );
        let time = frame_time();
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        Primitive::run(&mut prim, &mut ctx);

        for (slot, value) in render_mode_scratch.drain(..) {
            backend.set_render_mode(slot, value);
        }
        backend.render_mode(out_slot).expect("render_mode should be set")
    }

    #[test]
    fn unwired_defaults_produce_rendered() {
        let m = run_with(&[], &[]);
        assert_eq!(m.mode, 0, "default must be Rendered (INV-R1)");
        assert_eq!(m, RenderMode::default());
    }

    #[test]
    fn params_flow_through_to_render_mode_fields() {
        let m = run_with(&[
            ("mode", 2.0),
            ("clay_color_r", 0.25),
            ("line_color_g", 0.5),
            ("line_brightness", 3.0),
            ("point_size", 8.0),
        ], &[]);
        assert_eq!(m.mode, 2, "Wireframe");
        assert_eq!(m.clay_color, [0.25, 0.8, 0.8, 1.0]);
        assert_eq!(m.line_color, [0.1, 0.5, 1.0, 1.0]);
        assert_eq!(m.line_brightness, 3.0);
        assert_eq!(m.point_size, 8.0);
    }

    #[test]
    fn rendered_is_index_zero() {
        assert_eq!(RENDER_MODE_LABELS[0], "Rendered", "INV-R2: the enable gate multiplies enabled × mode, so index 0 must stay Rendered");
    }

    #[test]
    fn mode_scalar_rounds_and_clamps() {
        assert_eq!(run_with(&[("mode", 1.7)], &[]).mode, 2, "continuous wire values round");
        assert_eq!(run_with(&[("mode", 5.0)], &[]).mode, 3, "out of range clamps to Points");
        assert_eq!(run_with(&[("mode", -1.0)], &[]).mode, 0, "negative clamps to Rendered");
        assert_eq!(run_with(&[("mode", 2.0)], &[]).mode, 2);
    }

    #[test]
    fn brightness_and_point_size_clamp() {
        let hot = run_with(&[("line_brightness", 9.0), ("point_size", 99.0)], &[]);
        assert_eq!(hot.line_brightness, 4.0);
        assert_eq!(hot.point_size, 16.0);
        let cold = run_with(&[("line_brightness", -2.0), ("point_size", 0.0)], &[]);
        assert_eq!(cold.line_brightness, 0.0);
        assert_eq!(cold.point_size, 1.0);
    }

    #[test]
    fn wired_scalar_shadows_the_param() {
        // Port-shadow precedence: a wired `mode` scalar (the enable gate's
        // output) wins over the Enum param, exactly like fog's density.
        let m = run_with(&[("mode", 0.0)], &[("mode", 2.0)]);
        assert_eq!(m.mode, 2, "wired scalar must beat the param");
        let gated = run_with(&[("mode", 2.0)], &[("mode", 0.0)]);
        assert_eq!(gated.mode, 0, "gate at 0 resolves to Rendered (INV-R2)");
    }
}
