//! `node.free_camera` — free-look perspective camera source. Emits a single
//! [`Camera`] on the `out` port from world-space position + Euler angles
//! (yaw/pitch/roll) instead of an orbit target — the gizmo- and
//! import-friendly authoring mode every 3D tool ships alongside orbit.
//!
//! Seven port-shadowed scalar inputs (pos_x/pos_y/pos_z/yaw/pitch/roll/fov_y)
//! let the outer-card sliders — or a `beat_ramp` — drive the camera live, same
//! convention as `node.orbit_camera`. A dolly move becomes beat-addressable
//! for free. CPU-only — no GPU dispatch.

use std::borrow::Cow;

use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: FreeCamera,
    type_id: "node.free_camera",
    purpose: "Free-look perspective camera source. Emits one Camera on `out` from world-space position (pos_x/pos_y/pos_z) and Euler angles (yaw about world up Y, pitch about the camera's right axis, roll about fwd) — the gizmo- and import-friendly authoring mode, as opposed to node.orbit_camera's target-orbit style. At yaw=pitch=roll=0 the camera looks down -Z (right-handed convention). yaw/pitch/roll/fov_y are radians (Angle params; editor displays degrees). All seven spatial inputs are port-shadowed scalar inputs, so any of them can be driven by a beat_ramp or LFO — camera moves are beat-addressable. CPU-only, no GPU dispatch. Pair downstream with any 3D consumer (render_3d_mesh, render_instanced_3d_mesh, render_scene) that takes a `camera: Camera` input.",
    inputs: {
        pos_x: ScalarF32 optional,
        pos_y: ScalarF32 optional,
        pos_z: ScalarF32 optional,
        yaw: ScalarF32 optional,
        pitch: ScalarF32 optional,
        roll: ScalarF32 optional,
        fov_y: ScalarF32 optional,
    },
    outputs: {
        out: Camera,
        pos_x: ScalarF32,
        pos_y: ScalarF32,
        pos_z: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("pos_x"),
            label: "Pos X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("pos_y"),
            label: "Pos Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("pos_z"),
            label: "Pos Z",
            ty: ParamType::Float,
            default: ParamValue::Float(-3.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("yaw"),
            label: "Yaw",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("pitch"),
            label: "Pitch",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-1.5, 1.5)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("roll"),
            label: "Roll",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
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
            range: Some((0.001, 10.0)),
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
    composition_notes: "All seven spatial params (pos_x/pos_y/pos_z/yaw/pitch/roll/fov_y) are port-shadowed, so wiring a beat_ramp into any of them makes that axis of the camera move beat-addressable — a dolly move scrubbed by the timeline instead of hand-keyframed. yaw/pitch/roll/fov_y are Angle params, so the editor and outer-card sliders display and edit them in degrees while storage stays in radians. `near`/`far` rarely need outer-card control — leave defaults unless the scene has extreme depth. The output Camera carries the view matrix and projection-mode params; the projection matrix is built consumer-side via `cam.proj(target_aspect)`. `pos_x`/`pos_y`/`pos_z` outputs mirror `node.orbit_camera`'s surface for PBR material atoms that need the camera world position per pixel.",
    examples: [],
    picker: { label: "Free Camera", category: Driver },
    summary: "A free-look camera positioned and aimed directly with Euler angles, instead of orbiting a target. Gizmo- and import-friendly.",
    category: Geometry3D,
    role: Source,
    aliases: ["free camera", "fps camera", "euler camera", "dolly camera"],
    boundary_reason: NonGpu,
}

impl Primitive for FreeCamera {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pos_x = ctx.scalar_or_param("pos_x", 0.0);
        let pos_y = ctx.scalar_or_param("pos_y", 0.0);
        let pos_z = ctx.scalar_or_param("pos_z", -3.0);
        let yaw = ctx.scalar_or_param("yaw", 0.0);
        let pitch = ctx.scalar_or_param("pitch", 0.0);
        let roll = ctx.scalar_or_param("roll", 0.0);
        let fov_y = ctx.scalar_or_param("fov_y", 0.9).max(0.01);
        let near = match ctx.params.get("near") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.05,
        };
        let far = match ctx.params.get("far") {
            Some(ParamValue::Float(f)) => *f,
            _ => 200.0,
        };

        let cam = Camera::from_pos_euler([pos_x, pos_y, pos_z], yaw, pitch, roll, fov_y, near, far);
        let pos = cam.pos;
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
    fn free_camera_declares_seven_scalar_inputs_and_camera_output() {
        use crate::node_graph::ports::{PortType, ScalarType};

        assert_eq!(FreeCamera::TYPE_ID, "node.free_camera");
        let in_names: Vec<&str> = FreeCamera::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            in_names,
            vec!["pos_x", "pos_y", "pos_z", "yaw", "pitch", "roll", "fov_y"]
        );
        for input in FreeCamera::INPUTS {
            assert!(!input.required, "{} should be optional (port-shadow)", input.name);
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(FreeCamera::OUTPUTS.len(), 4);
        assert_eq!(FreeCamera::OUTPUTS[0].name, "out");
        assert_eq!(FreeCamera::OUTPUTS[0].ty, PortType::Camera);
        assert_eq!(FreeCamera::OUTPUTS[1].name, "pos_x");
        assert_eq!(FreeCamera::OUTPUTS[1].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(FreeCamera::OUTPUTS[2].name, "pos_y");
        assert_eq!(FreeCamera::OUTPUTS[3].name, "pos_z");
    }

    #[test]
    fn free_camera_has_full_param_surface() {
        let names: Vec<&str> = FreeCamera::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec![
                "pos_x", "pos_y", "pos_z", "yaw", "pitch", "roll", "fov_y", "near", "far"
            ]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = FreeCamera::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.free_camera");
    }
}
