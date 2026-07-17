//! `node.transform_3d` — TRS (position / rotation / scale) producer for
//! scene objects.
//!
//! Per `docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` §2 D2: nine scalar
//! params (position, Euler rotation in radians, scale) mirroring
//! `render_scene`'s current per-object transform params verbatim (same
//! labels, ranges, `ParamType::Angle` for rotation), each port-shadowed by a
//! same-named optional scalar input port per
//! `project_control_wires_port_shadows_param` (prefer `ctx.inputs.scalar`,
//! fall back to the param). Outputs a single [`Transform`] struct consumed
//! by `render_scene`'s `transform_n` ports (P2 of the design) instead of
//! nine separate per-object params — closing REALTIME_3D's "transforms not
//! beat-addressable" gap: an LFO wired to `rot_y` is a spinning object; a
//! `beat_ramp` into `pos_z` is a drop hit.
//!
//! CPU-only — no GPU dispatch. The wire carries plain TRS; matrices are
//! composed by consumers (`render_scene`'s existing `model_matrix`), never
//! on the wire itself.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::transform::Transform;

crate::primitive! {
    name: Transform3D,
    type_id: "node.transform_3d",
    purpose: "TRS (position/rotation/scale) producer for scene objects. Nine scalar params — position X/Y/Z, rotation X/Y/Z (radians), scale X/Y/Z — each port-shadowed by a same-named optional scalar input port so the transform can be driven by an LFO, MIDI, a beat_ramp, or any other control-rate source. Outputs a single Transform struct (pos/rot_euler/scale) consumed by render_scene's transform_n ports (replacing nine per-object params) or any future TRS consumer. Matrices are composed by consumers — the wire carries plain TRS, never a matrix.",
    inputs: {
        pos_x: ScalarF32 optional,
        pos_y: ScalarF32 optional,
        pos_z: ScalarF32 optional,
        rot_x: ScalarF32 optional,
        rot_y: ScalarF32 optional,
        rot_z: ScalarF32 optional,
        scale_x: ScalarF32 optional,
        scale_y: ScalarF32 optional,
        scale_z: ScalarF32 optional,
    },
    outputs: {
        transform: Transform,
    },
    params: [
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
            default: ParamValue::Float(0.0),
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
            name: Cow::Borrowed("rot_x"),
            label: "Rotation X",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rot_y"),
            label: "Rotation Y",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rot_z"),
            label: "Rotation Z",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scale_x"),
            label: "Scale X",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.01, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scale_y"),
            label: "Scale Y",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.01, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scale_z"),
            label: "Scale Z",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.01, 10.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire `transform` into render_scene's transform_n port (unwired = identity: pos 0, rot 0, scale 1). Rotation is XYZ Euler in radians, matching render_scene's existing model_matrix. Each of the nine params is independently port-shadowed — wire only the axes you want to animate; the rest fall back to their static param values.",
    examples: [],
    picker: { label: "Transform 3D", category: Driver },
    summary: "Position, rotation, and scale for one scene object. Wire it into a render_scene transform slot, or drive an axis from an LFO or MIDI to animate it live.",
    category: Geometry3D,
    role: Source,
    aliases: ["transform", "transform 3d", "trs", "position rotation scale", "object transform"],
    boundary_reason: NonGpu,
}

impl Primitive for Transform3D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pos_x = ctx.scalar_or_param("pos_x", 0.0);
        let pos_y = ctx.scalar_or_param("pos_y", 0.0);
        let pos_z = ctx.scalar_or_param("pos_z", 0.0);
        let rot_x = ctx.scalar_or_param("rot_x", 0.0);
        let rot_y = ctx.scalar_or_param("rot_y", 0.0);
        let rot_z = ctx.scalar_or_param("rot_z", 0.0);
        let scale_x = ctx.scalar_or_param("scale_x", 1.0);
        let scale_y = ctx.scalar_or_param("scale_y", 1.0);
        let scale_z = ctx.scalar_or_param("scale_z", 1.0);

        let transform = Transform {
            pos: [pos_x, pos_y, pos_z],
            rot_euler: [rot_x, rot_y, rot_z],
            scale: [scale_x, scale_y, scale_z],
        };

        ctx.outputs.set_transform("transform", transform);
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
    use crate::node_graph::ports::{PortType, ScalarType};
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
    fn transform_3d_declares_nine_port_shadow_scalars_and_transform_output() {
        assert_eq!(Transform3D::TYPE_ID, "node.transform_3d");
        for input in Transform3D::INPUTS {
            assert!(!input.required, "{} should be optional (port-shadow)", input.name);
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(Transform3D::INPUTS.len(), 9);
        assert_eq!(Transform3D::OUTPUTS.len(), 1);
        assert_eq!(Transform3D::OUTPUTS[0].name, "transform");
        assert_eq!(Transform3D::OUTPUTS[0].ty, PortType::Transform);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Transform3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.transform_3d");
    }

    /// Runs `Transform3D` with the given params (defaults inserted for any
    /// name not overridden) and no wired scalar ports, returning the
    /// resulting `Transform`.
    fn run_with_params(overrides: &[(&'static str, f32)]) -> Transform {
        run_with_params_and_wires(overrides, &[])
    }

    /// Runs `Transform3D` with the given params and the given wired scalar
    /// ports (name, wired value) — the wire wins over the param for any
    /// name present in `wires`.
    fn run_with_params_and_wires(
        overrides: &[(&'static str, f32)],
        wires: &[(&'static str, f32)],
    ) -> Transform {
        let defaults: &[(&str, f32)] = &[
            ("pos_x", 0.0),
            ("pos_y", 0.0),
            ("pos_z", 0.0),
            ("rot_x", 0.0),
            ("rot_y", 0.0),
            ("rot_z", 0.0),
            ("scale_x", 1.0),
            ("scale_y", 1.0),
            ("scale_z", 1.0),
        ];

        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::Transform, None, (0, 0));

        let mut params = ParamValues::default();
        for &(name, default) in defaults {
            let value = overrides
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| *v)
                .unwrap_or(default);
            params.insert(Cow::Owned(name.to_string()), ParamValue::Float(value));
        }

        let mut wire_slots: Vec<(&'static str, Slot)> = Vec::new();
        let mut next_id = 1u32;
        for &(name, value) in wires {
            let slot = backend.acquire(
                ResourceId(next_id),
                PortType::Scalar(ScalarType::F32),
                None,
                (0, 0),
            );
            next_id += 1;
            backend.set_scalar(slot, ParamValue::Float(value));
            wire_slots.push((name, slot));
        }

        let mut prim = Transform3D::new();
        let outputs_bindings: &[(&'static str, Slot)] = &[("transform", out_slot)];
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
    fn unwired_defaults_produce_identity_transform() {
        let t = run_with_params(&[]);
        assert_eq!(t, Transform::default());
    }

    #[test]
    fn params_flow_through_to_matching_transform_fields_radians_preserved() {
        let t = run_with_params(&[
            ("pos_x", 1.5),
            ("pos_y", -2.0),
            ("pos_z", 3.25),
            ("rot_x", 0.5),
            ("rot_y", std::f32::consts::PI),
            ("rot_z", -1.0),
            ("scale_x", 2.0),
            ("scale_y", 0.5),
            ("scale_z", 4.0),
        ]);
        assert_eq!(t.pos, [1.5, -2.0, 3.25]);
        assert_eq!(t.rot_euler, [0.5, std::f32::consts::PI, -1.0]);
        assert_eq!(t.scale, [2.0, 0.5, 4.0]);
    }

    #[test]
    fn wired_pos_x_overrides_its_same_named_param() {
        // Param says 10.0; the wire says 99.0 — the wire must win
        // (port-shadows-param).
        let t = run_with_params_and_wires(&[("pos_x", 10.0)], &[("pos_x", 99.0)]);
        assert_eq!(t.pos[0], 99.0, "wired pos_x should override the pos_x param");
        // Untouched siblings keep their param/default values.
        assert_eq!(t.pos[1], 0.0);
        assert_eq!(t.pos[2], 0.0);
    }

    #[test]
    fn wired_rot_y_overrides_its_same_named_param() {
        let t = run_with_params_and_wires(&[("rot_y", 1.0)], &[("rot_y", 2.5)]);
        assert_eq!(t.rot_euler[1], 2.5, "wired rot_y should override the rot_y param");
        assert_eq!(t.rot_euler[0], 0.0);
        assert_eq!(t.rot_euler[2], 0.0);
    }

    #[test]
    fn wired_scale_z_overrides_its_same_named_param() {
        let t = run_with_params_and_wires(&[("scale_z", 3.0)], &[("scale_z", 7.0)]);
        assert_eq!(t.scale[2], 7.0, "wired scale_z should override the scale_z param");
        assert_eq!(t.scale[0], 1.0);
        assert_eq!(t.scale[1], 1.0);
    }
}
