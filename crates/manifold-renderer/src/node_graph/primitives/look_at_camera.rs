//! `node.look_at_camera` — look-at perspective camera source. Emits a single
//! [`Camera`] on the `out` port from world-space position + target instead of
//! an orbit or Euler-angle authoring mode — the third standard camera
//! parameterization every 3D tool ships (alongside orbit and free-look).
//!
//! Seven port-shadowed scalar inputs (pos_x/pos_y/pos_z/target_x/target_y/
//! target_z/fov_y) let the outer-card sliders — or a `beat_ramp` — drive the
//! camera live, same convention as `node.orbit_camera` / `node.free_camera`.
//! World up `(0,1,0)` is fixed in v1 — no up param. CPU-only — no GPU
//! dispatch.

use std::borrow::Cow;
use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: LookAtCamera,
    type_id: "node.look_at_camera",
    purpose: "Look-at perspective camera source. Emits one Camera on `out` from a world-space position (pos_x/pos_y/pos_z) and a world-space target point (target_x/target_y/target_z) — fwd = normalize(target - pos), right/up orthonormalized against fixed world up (0,1,0). The third standard camera authoring mode alongside node.orbit_camera's target-orbit style and node.free_camera's Euler pos+angles. fov_y is radians (Angle param; editor displays degrees). All seven spatial inputs are port-shadowed scalar inputs, so any of them can be driven by a beat_ramp or LFO — camera moves are beat-addressable. CPU-only, no GPU dispatch. Pair downstream with any 3D consumer (render_3d_mesh, render_instanced_3d_mesh, render_scene) that takes a `camera: Camera` input.",
    inputs: {
        pos_x: ScalarF32 optional,
        pos_y: ScalarF32 optional,
        pos_z: ScalarF32 optional,
        target_x: ScalarF32 optional,
        target_y: ScalarF32 optional,
        target_z: ScalarF32 optional,
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
            name: Cow::Borrowed("target_x"),
            label: "Target X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("target_y"),
            label: "Target Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("target_z"),
            label: "Target Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
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
    composition_notes: "All seven spatial params (pos_x/pos_y/pos_z/target_x/target_y/target_z/fov_y) are port-shadowed, so wiring a beat_ramp into any of them makes that axis of the camera move beat-addressable. World up is fixed at (0,1,0) in v1 — no up param; a near-vertical pos-target line will degenerate the basis the same way any look-at camera does. fov_y is an Angle param, so the editor and outer-card sliders display and edit it in degrees while storage stays in radians. `near`/`far` rarely need outer-card control — leave defaults unless the scene has extreme depth. The output Camera carries the view matrix and projection-mode params; the projection matrix is built consumer-side via `cam.proj(target_aspect)`. `pos_x`/`pos_y`/`pos_z` outputs mirror `node.orbit_camera`'s surface for PBR material atoms that need the camera world position per pixel.",
    examples: [],
    picker: { label: "Look-At Camera", category: Driver },
    summary: "A camera positioned directly and aimed at a target point, instead of orbiting or using Euler angles.",
    category: Geometry3D,
    role: Source,
    aliases: ["look at camera", "target camera", "aim camera"],
    boundary_reason: NonGpu,
}

impl Primitive for LookAtCamera {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pos_x = ctx.scalar_or_param("pos_x", 0.0);
        let pos_y = ctx.scalar_or_param("pos_y", 0.0);
        let pos_z = ctx.scalar_or_param("pos_z", -3.0);
        let target_x = ctx.scalar_or_param("target_x", 0.0);
        let target_y = ctx.scalar_or_param("target_y", 0.0);
        let target_z = ctx.scalar_or_param("target_z", 0.0);
        let fov_y = ctx.scalar_or_param("fov_y", 0.9).max(0.01);
        let near = match ctx.params.get("near") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.05,
        };
        let far = match ctx.params.get("far") {
            Some(ParamValue::Float(f)) => *f,
            _ => 200.0,
        };

        let cam = Camera::look_at(
            [pos_x, pos_y, pos_z],
            [target_x, target_y, target_z],
            [0.0, 1.0, 0.0],
            fov_y,
            near,
            far,
        );
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
    fn look_at_camera_declares_seven_scalar_inputs_and_camera_output() {
        use crate::node_graph::ports::{PortType, ScalarType};

        assert_eq!(LookAtCamera::TYPE_ID, "node.look_at_camera");
        let in_names: Vec<&str> = LookAtCamera::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            in_names,
            vec!["pos_x", "pos_y", "pos_z", "target_x", "target_y", "target_z", "fov_y"]
        );
        for input in LookAtCamera::INPUTS {
            assert!(!input.required, "{} should be optional (port-shadow)", input.name);
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(LookAtCamera::OUTPUTS.len(), 4);
        assert_eq!(LookAtCamera::OUTPUTS[0].name, "out");
        assert_eq!(LookAtCamera::OUTPUTS[0].ty, PortType::Camera);
        assert_eq!(LookAtCamera::OUTPUTS[1].name, "pos_x");
        assert_eq!(LookAtCamera::OUTPUTS[1].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(LookAtCamera::OUTPUTS[2].name, "pos_y");
        assert_eq!(LookAtCamera::OUTPUTS[3].name, "pos_z");
    }

    #[test]
    fn look_at_camera_has_full_param_surface() {
        let names: Vec<&str> = LookAtCamera::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec![
                "pos_x", "pos_y", "pos_z", "target_x", "target_y", "target_z", "fov_y", "near", "far"
            ]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = LookAtCamera::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.look_at_camera");
    }
}
