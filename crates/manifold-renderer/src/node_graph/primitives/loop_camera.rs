//! `node.loop_camera` — beat-locked flythrough camera for scene looping
//! (SCENE_LOOP_DESIGN.md D3).
//!
//! Emits a single [`Camera`] on `out`. Position advances `phase * cell_size`
//! along the chosen axis; look direction is travel-aligned. `phase` (0..1)
//! comes from a `beat_ramp` at attack=1, rate=1/bars — the frame at phase 0
//! is identical to the frame at phase 1 (INV-3 wrap purity).
//!
//! CPU-only — no GPU dispatch. Same convention as `node.orbit_camera`.

use std::borrow::Cow;

use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const LOOP_CAMERA_AXIS_LABELS: &[&str] = &["+X", "-X", "+Y", "-Y", "+Z", "-Z"];

crate::primitive! {
    name: LoopCamera,
    type_id: "node.loop_camera",
    purpose: "Beat-locked flythrough camera for scene looping. Emits one Camera on `out` from phase (0..1, wired from beat_ramp at attack=1 rate=1/bars), cell_size, axis (+X/-X/+Y/-Y/+Z/-Z), lateral/height offsets, and fov. Position advances phase * cell_size along axis; look direction is travel-aligned (down the axis). The frame at phase 0 equals the frame at phase 1 by construction (INV-3 wrap purity). CPU-only, no GPU dispatch.",
    inputs: {
        phase: ScalarF32 optional,
    },
    outputs: {
        out: Camera,
        pos_x: ScalarF32,
        pos_y: ScalarF32,
        pos_z: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("cell_size"),
            label: "Cell Size",
            ty: ParamType::Float,
            default: ParamValue::Float(10.0),
            range: Some((0.01, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("axis"),
            label: "Axis",
            ty: ParamType::Enum,
            default: ParamValue::Enum(4), // +Z
            range: None,
            enum_values: LOOP_CAMERA_AXIS_LABELS,
        },
        ParamDef {
            name: Cow::Borrowed("lateral"),
            label: "Lateral",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("height"),
            label: "Height",
            ty: ParamType::Float,
            default: ParamValue::Float(1.5),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("home"),
            label: "Home",
            // Phase-0 start along the travel axis (SCENE_LOOP_DESIGN D10 +
            // the BUG-70wo gap rule): the plan builder's cell is two
            // object-depths and home = -cell/2 puts the camera mid-gap, one
            // half-depth of air in front of copy 0's near face — the phase-0
            // frame is an approach shot, wrap-identical to phase 1.
            // Default 0 keeps the primitive's standalone behaviour unchanged.
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100000.0, 100000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("fov_y"),
            label: "FOV Y",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.9),
            range: Some((0.05, 2.5)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("near"),
            label: "Near",
            ty: ParamType::Float,
            default: ParamValue::Float(0.05),
            range: Some((0.001, 10000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("far"),
            label: "Far",
            ty: ParamType::Float,
            default: ParamValue::Float(200.0),
            range: Some((1.0, 10000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "phase is port-shadowed: wire from beat_ramp (attack=1, rate=1/bars) for beat-locked looping. Unwired, reads FrameTime.beats mod 1. The SAME cell_size feeds both this node and node.scene_array — the plan builder computes it once from scene_bounds (D4). lateral/height offset the camera within the cross-section perpendicular to travel. axis enum matches scene_array's axis enum. Camera looks down the travel axis (fwd = axis direction); lateral offsets the camera perpendicular to travel. pos_x/pos_y/pos_z outputs for PBR material atoms.",
    examples: [],
    picker: { label: "Loop Camera", category: Driver },
    summary: "A camera that flies through a scene in a perfect loop, locked to the beat.",
    category: Geometry3D,
    role: Source,
    aliases: ["loop camera", "flythrough camera", "travel camera"],
    boundary_reason: NonGpu,
}

impl Primitive for LoopCamera {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cell_size = match ctx.params.get("cell_size") {
            Some(ParamValue::Float(f)) => *f,
            _ => 10.0,
        };
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 4, // +Z
        };
        let lateral = ctx.scalar_or_param("lateral", 0.0);
        let height = ctx.scalar_or_param("height", 1.5);
        // home comes from the plan builder (-cell/2 = mid-gap start under the
        // D4 gap rule); standalone default 0 starts at the origin.
        let home = ctx.scalar_or_param("home", 0.0);
        let fov_y = ctx.scalar_or_param("fov_y", 0.9).max(0.01);
        let near = match ctx.params.get("near") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.05,
        };
        let far = match ctx.params.get("far") {
            Some(ParamValue::Float(f)) => *f,
            _ => 200.0,
        };

        // Phase: wired beat_ramp or fallback to fract(beats).
        let phase = match ctx.inputs.scalar("phase") {
            Some(ParamValue::Float(f)) => f.fract().clamp(0.0, 1.0),
            _ => {
                let b = ctx.time.beats.0 as f32;
                b.fract().clamp(0.0, 1.0)
            }
        };

        // Travel distance along the axis, from the corridor entry `home`.
        let travel = home + phase * cell_size;

        // Build position: travel along axis + lateral/height offsets.
        // Lateral offsets perpendicular to travel; height is Y offset.
        let (pos, fwd): ([f32; 3], [f32; 3]) = match axis {
            0 => ([travel, height, lateral], [1.0, 0.0, 0.0]),       // +X
            1 => ([-travel, height, lateral], [-1.0, 0.0, 0.0]),     // -X
            2 => ([lateral, travel, height], [0.0, 1.0, 0.0]),       // +Y
            3 => ([lateral, -travel, height], [0.0, -1.0, 0.0]),     // -Y
            4 => ([lateral, height, travel], [0.0, 0.0, 1.0]),       // +Z
            _ => ([lateral, height, -travel], [0.0, 0.0, -1.0]),     // -Z
        };

        // Target is one unit ahead along the travel axis.
        let target = [pos[0] + fwd[0], pos[1] + fwd[1], pos[2] + fwd[2]];
        let cam = Camera::look_at(pos, target, [0.0, 1.0, 0.0], fov_y, near, far);

        ctx.outputs.set_camera("out", cam);
        ctx.outputs.set_scalar("pos_x", ParamValue::Float(pos[0]));
        ctx.outputs.set_scalar("pos_y", ParamValue::Float(pos[1]));
        ctx.outputs.set_scalar("pos_z", ParamValue::Float(pos[2]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn loop_camera_declares_one_scalar_input_and_camera_output() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(LoopCamera::TYPE_ID, "node.loop_camera");
        let in_names: Vec<&str> = LoopCamera::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(in_names, vec!["phase"]);
        for input in LoopCamera::INPUTS {
            assert!(!input.required, "{} should be optional (port-shadow)", input.name);
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(LoopCamera::OUTPUTS.len(), 4);
        assert_eq!(LoopCamera::OUTPUTS[0].name, "out");
        assert_eq!(LoopCamera::OUTPUTS[0].ty, PortType::Camera);
        assert_eq!(LoopCamera::OUTPUTS[1].name, "pos_x");
        assert_eq!(LoopCamera::OUTPUTS[2].name, "pos_y");
        assert_eq!(LoopCamera::OUTPUTS[3].name, "pos_z");
    }

    #[test]
    fn loop_camera_has_eight_params() {
        let names: Vec<&str> = LoopCamera::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["cell_size", "axis", "lateral", "height", "home", "fov_y", "near", "far"]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = LoopCamera::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.loop_camera");
    }
}
