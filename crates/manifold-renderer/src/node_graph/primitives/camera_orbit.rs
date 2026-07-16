//! `node.orbit_camera` — orbit-style perspective camera source. Emits a
//! single [`Camera`] on the `out` port; consumers (3D mesh renderers,
//! particle-camera splat) take it as one input port instead of N separate
//! scalar params.
//!
//! Five port-shadowed scalar inputs (orbit, tilt, distance, fov, look_y) let
//! the outer-card sliders drive the camera live — same convention every
//! existing 3D primitive uses for its individual camera params, now collapsed
//! into one node so the math lives in one place. CPU-only — no GPU dispatch.
//!
//! The orbit math is the bit-exact formula every legacy renderer used
//! pre-Camera-port:
//! ```text
//! pos = (
//!     distance * cos(orbit) * cos(tilt),
//!     distance * sin(tilt) + look_y,
//!     distance * sin(orbit) * cos(tilt),
//! )
//! ```
//! Target is `(0, look_y, 0)`. World up is `(0, 1, 0)`.

use std::borrow::Cow;

use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Single source of truth for `near`'s default (also the `ParamDef` default
/// below) — `gltf_import.rs` reads this to scale the clip plane for
/// small-scale imported objects (BUG-165/BUG-169) instead of duplicating
/// the magic number.
pub const DEFAULT_NEAR: f32 = 0.05;

crate::primitive! {
    name: CameraOrbit,
    type_id: "node.orbit_camera",
    purpose: "Orbit-style perspective camera source. Emits one Camera on `out` from five port-shadowed scalar inputs (orbit/tilt/distance/fov_y/look_y): pos = (distance*cos(orbit)*cos(tilt), distance*sin(tilt) + look_y, distance*sin(orbit)*cos(tilt)); target = (0, look_y, 0); world up = (0, 1, 0) — the bit-exact formula every legacy 3D renderer used pre-Camera-port. orbit/tilt/fov_y/roll are radians (Angle params; editor displays degrees), distance ∈ (0.01, 100], fov_y ∈ [0.05, 2.5] rad, look_y ∈ [-10, 10]. Also exposes pos_x/pos_y/pos_z scalar outputs for PBR shading atoms that need the camera world position per pixel. CPU-only, no GPU dispatch. Pair downstream with any 3D consumer (render_3d_mesh, render_instanced_3d_mesh, digital_plants_render, scatter_particles_camera) that takes a `camera: Camera` input — replaces N separate camera scalar params per primitive with one wire.",
    inputs: {
        orbit: ScalarF32 optional,
        tilt: ScalarF32 optional,
        distance: ScalarF32 optional,
        fov_y: ScalarF32 optional,
        look_y: ScalarF32 optional,
        roll: ScalarF32 optional,
    },
    outputs: {
        out: Camera,
        pos_x: ScalarF32,
        pos_y: ScalarF32,
        pos_z: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("orbit"),
            label: "Orbit",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.7),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("tilt"),
            label: "Tilt",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.3),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("distance"),
            label: "Distance",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((0.01, 100.0)),
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
            name: Cow::Borrowed("look_y"),
            label: "Look Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-10.0, 10.0)),
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
            name: Cow::Borrowed("near"),
            label: "Near",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_NEAR),
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
    composition_notes: "orbit / tilt / roll / fov_y are angle params, so the editor and outer-card sliders display and edit them in degrees natively while storage stays in radians. Expose them directly. Older presets like DigitalPlants / MetallicGlass instead wire a `node.scale_offset_value` (scale = π/180) ahead of each angle input to turn a degrees-valued driver into radians, which still works. `near` / `far` rarely need outer-card control — leave defaults unless the scene has extreme depth. The output Camera carries the view matrix and projection-mode params; the projection matrix is built consumer-side via `cam.proj(target_aspect)` because the aspect depends on the consumer's render target. `pos_x`/`pos_y`/`pos_z` outputs are the camera's world position — `node.render_mesh` takes the Camera directly and derives the per-pixel view vector (V = camera_pos - world_pos) internally for its PBR material.",
    examples: [],
    picker: { label: "Orbit Camera", category: Driver },
    summary: "A camera that orbits around a target point, with controls for distance, height, and angle. The viewpoint for 3D mesh rendering.",
    category: Geometry3D,
    role: Source,
    aliases: ["orbit camera", "camera orbit", "camera", "viewpoint", "Camera COMP"],
    boundary_reason: NonGpu,
}

impl Primitive for CameraOrbit {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let orbit = ctx.scalar_or_param("orbit", 0.7);
        let tilt = ctx.scalar_or_param("tilt", 0.3);
        let distance = ctx.scalar_or_param("distance", 4.0).max(0.001);
        let fov_y = ctx.scalar_or_param("fov_y", 0.9).max(0.01);
        let look_y = ctx.scalar_or_param("look_y", 0.0);
        let roll = ctx.scalar_or_param("roll", 0.0);
        let near = match ctx.params.get("near") {
            Some(ParamValue::Float(f)) => *f,
            _ => DEFAULT_NEAR,
        };
        let far = match ctx.params.get("far") {
            Some(ParamValue::Float(f)) => *f,
            _ => 200.0,
        };

        let cam = Camera::orbit_perspective(orbit, tilt, distance, fov_y, look_y, roll, near, far);
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
    fn camera_orbit_declares_six_scalar_inputs_and_camera_output() {
        use crate::node_graph::ports::{PortType, ScalarType};

        assert_eq!(CameraOrbit::TYPE_ID, "node.orbit_camera");
        let in_names: Vec<&str> = CameraOrbit::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(in_names, vec!["orbit", "tilt", "distance", "fov_y", "look_y", "roll"]);
        for input in CameraOrbit::INPUTS {
            assert!(!input.required, "{} should be optional (port-shadow)", input.name);
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(CameraOrbit::OUTPUTS.len(), 4);
        assert_eq!(CameraOrbit::OUTPUTS[0].name, "out");
        assert_eq!(CameraOrbit::OUTPUTS[0].ty, PortType::Camera);
        assert_eq!(CameraOrbit::OUTPUTS[1].name, "pos_x");
        assert_eq!(CameraOrbit::OUTPUTS[1].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(CameraOrbit::OUTPUTS[2].name, "pos_y");
        assert_eq!(CameraOrbit::OUTPUTS[3].name, "pos_z");
    }

    #[test]
    fn camera_orbit_has_full_param_surface() {
        let names: Vec<&str> = CameraOrbit::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["orbit", "tilt", "distance", "fov_y", "look_y", "roll", "near", "far"]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = CameraOrbit::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.orbit_camera");
    }
}
