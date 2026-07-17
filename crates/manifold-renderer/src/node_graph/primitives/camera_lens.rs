//! `node.camera_lens` — physical lens rewriter for a `Camera` wire.
//!
//! Pure CPU pass-through (no GPU dispatch): takes a `Camera` in, rewrites
//! ONLY its `lens` block (`focus_distance` / `f_stop` / `shutter_angle` /
//! `exposure_ev`) from four port-shadowed scalar params, and passes every
//! other field (position, basis, near/far, projection mode, cached view
//! matrix) through unchanged. This is "the one lens" of
//! `docs/CAMERA_AND_LENS_DESIGN.md` §2 D4 — insert it once between a camera
//! source (`node.orbit_camera` / `node.free_camera` / `node.look_at_camera`)
//! and its consumers so depth-of-field (CINEMATIC_POST's `coc_from_depth`),
//! motion blur (CINEMATIC_POST's `motion_blur`), and exposure
//! (`node.render_scene`, D5) all read the same lens instead of duplicating
//! four params per consumer.
//!
//! Defaults are chosen to make an unwired, untouched `camera_lens` node a
//! no-op: `focus_distance = 0` (neutral, per `LensParams`'s `<= 0` =
//! hyperfocal contract), `shutter_angle = 0` (no motion blur), `exposure_ev
//! = 0` (neutral — `render_scene` multiplies by `exp2(0) = 1`). `f_stop`'s
//! *param* default is `1000.0`, NOT `LensParams::PINHOLE`'s literal
//! `f32::INFINITY` — `f32::INFINITY` is safe as a Rust const on the
//! never-serialized `Camera`/`LensParams` wire types, but this primitive's
//! params ARE serialized like any other param, and `serde_json` silently
//! encodes non-finite floats as JSON `null` and then fails to decode `null`
//! back into an `f32` on load (verified empirically against this
//! workspace's `serde_json` version — not assumed). An f-stop of 1000 is
//! optically indistinguishable from a pinhole for any circle-of-confusion
//! formula (CoC scales with `1/f_stop`) while round-tripping through
//! project save/load like every other param.

use std::borrow::Cow;

use crate::node_graph::camera::{Camera, LensParams};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: CameraLens,
    type_id: "node.camera_lens",
    purpose: "Physical lens for a Camera wire: focus_distance / f_stop / shutter_angle / exposure_ev, all four port-shadowed scalar params. Pure CPU pass-through — takes a Camera in, rewrites ONLY its lens block, passes every other field (position, basis vectors, near/far, projection mode, cached view matrix) through unchanged. Insert once between a camera source (node.orbit_camera / node.free_camera / node.look_at_camera) and its consumers so depth-of-field, motion blur, and node.render_scene's exposure all read the same lens instead of duplicating params. focus_distance is world units along the camera's fwd axis (<= 0 = hyperfocal/neutral); f_stop is the aperture N (large = shallow-DoF off); shutter_angle is degrees 0..=360 (0 = no motion blur); exposure_ev is stops (0 = neutral, render_scene multiplies its final straight rgb by 2^exposure_ev). Defaults reproduce a neutral lens, so an unwired, untouched camera_lens node changes nothing downstream.",
    inputs: {
        camera: Camera required,
        focus_distance: ScalarF32 optional,
        f_stop: ScalarF32 optional,
        shutter_angle: ScalarF32 optional,
        exposure_ev: ScalarF32 optional,
    },
    outputs: {
        out: Camera,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("focus_distance"),
            label: "Focus Distance",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("f_stop"),
            label: "F-Stop",
            ty: ParamType::Float,
            // 1000.0, not f32::INFINITY: see the module doc comment — this
            // value is serialized like any param, and serde_json silently
            // corrupts non-finite floats on save/load. 1000 is optically a
            // pinhole for any 1/f_stop CoC formula.
            default: ParamValue::Float(1000.0),
            range: Some((0.5, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("shutter_angle"),
            label: "Shutter Angle",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 360.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("exposure_ev"),
            label: "Exposure (EV)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-8.0, 8.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "All four params are port-shadowed — wire an LFO into exposure_ev for a strobe, a fader into f_stop to rack focus (once CINEMATIC_POST's coc_from_depth reads it), a drop macro into shutter_angle for a motion-blur smear. Defaults are neutral, so dropping this node with every slider untouched changes nothing downstream. node.render_scene multiplies its final straight rgb by exp2(exposure_ev) every frame (docs/CAMERA_AND_LENS_DESIGN.md D5) — that's live today. focus_distance/f_stop/shutter_angle are read by CINEMATIC_POST's DoF/motion-blur atoms once those ship; wiring this node ahead of time is harmless.",
    examples: [],
    picker: { label: "Camera Lens", category: Atom },
    summary: "The physical camera: focus distance, aperture, shutter angle, and exposure — one lens any camera source can feed, and every 3D consumer reads.",
    category: Geometry3D,
    role: Filter,
    aliases: ["camera lens", "lens", "aperture", "exposure", "depth of field", "dof", "f-stop", "shutter angle"],
    boundary_reason: NonGpu,
}

impl Primitive for CameraLens {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(cam) = ctx.inputs.camera("camera") else {
            return;
        };
        let focus_distance = ctx.scalar_or_param("focus_distance", 0.0);
        let f_stop = ctx.scalar_or_param("f_stop", 1000.0);
        let shutter_angle = ctx.scalar_or_param("shutter_angle", 0.0);
        let exposure_ev = ctx.scalar_or_param("exposure_ev", 0.0);

        let out = Camera {
            lens: LensParams {
                focus_distance,
                f_stop,
                shutter_angle,
                exposure_ev,
            },
            ..cam
        };
        ctx.outputs.set_camera("out", out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_camera_in_out_and_four_port_shadow_scalars() {
        use crate::node_graph::ports::{PortType, ScalarType};

        assert_eq!(CameraLens::TYPE_ID, "node.camera_lens");
        let in_names: Vec<&str> = CameraLens::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            in_names,
            vec!["camera", "focus_distance", "f_stop", "shutter_angle", "exposure_ev"]
        );
        assert_eq!(CameraLens::INPUTS[0].ty, PortType::Camera);
        assert!(CameraLens::INPUTS[0].required);
        for input in &CameraLens::INPUTS[1..] {
            assert!(!input.required, "{} should be optional (port-shadow)", input.name);
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
        }

        assert_eq!(CameraLens::OUTPUTS.len(), 1);
        assert_eq!(CameraLens::OUTPUTS[0].name, "out");
        assert_eq!(CameraLens::OUTPUTS[0].ty, PortType::Camera);
    }

    #[test]
    fn has_four_lens_params() {
        let names: Vec<&str> = CameraLens::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["focus_distance", "f_stop", "shutter_angle", "exposure_ev"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = CameraLens::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.camera_lens");
    }
}

#[cfg(test)]
mod run_tests {
    //! `Primitive::run` behavior via `MockBackend` — the same harness shape
    //! as `transform_3d.rs`'s `run_with_params_and_wires` (Camera substituted
    //! for Transform). Proves: (a) the incoming camera's non-lens fields
    //! pass through unchanged, (b) unwired params write a neutral lens, (c)
    //! a wired scalar overrides its same-named param (port-shadow
    //! precedence, D4).
    use super::*;
    use crate::node_graph::MockBackend;
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

    /// Runs `CameraLens` against `input_cam` with the given param overrides
    /// (defaults inserted for any name not overridden) and the given wired
    /// scalar ports (name, wired value) — the wire wins over the param for
    /// any name present in `wires`. Returns the resulting `Camera`.
    fn run_camera_lens(
        input_cam: Camera,
        overrides: &[(&'static str, f32)],
        wires: &[(&'static str, f32)],
    ) -> Camera {
        let defaults: &[(&str, f32)] = &[
            ("focus_distance", 0.0),
            ("f_stop", 1000.0),
            ("shutter_angle", 0.0),
            ("exposure_ev", 0.0),
        ];

        let mut backend = MockBackend::new();
        let cam_slot = backend.acquire(ResourceId(0), PortType::Camera, None, (0, 0));
        backend.set_camera(cam_slot, input_cam);
        let out_slot = backend.acquire(ResourceId(1), PortType::Camera, None, (0, 0));

        let mut params = ParamValues::default();
        for &(name, default) in defaults {
            let value = overrides
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| *v)
                .unwrap_or(default);
            params.insert(Cow::Owned(name.to_string()), ParamValue::Float(value));
        }

        let mut wire_slots: Vec<(&'static str, Slot)> = vec![("camera", cam_slot)];
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

        let mut prim = CameraLens::new();
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

        for (slot, value) in camera_scratch.drain(..) {
            backend.set_camera(slot, value);
        }

        backend.camera(out_slot).expect("camera should be set")
    }

    #[test]
    fn unwired_defaults_produce_a_neutral_lens_matching_pinhole() {
        let input_cam = Camera::default_perspective();
        let out = run_camera_lens(input_cam, &[], &[]);
        assert_eq!(out.lens.focus_distance, 0.0);
        assert_eq!(out.lens.shutter_angle, 0.0);
        assert_eq!(out.lens.exposure_ev, 0.0);
        // f_stop's serialization-safe param default (1000.0) is not
        // literally infinite, but is optically pinhole-equivalent.
        assert!(out.lens.f_stop >= 1000.0);
    }

    #[test]
    fn non_lens_fields_pass_through_unchanged() {
        let input_cam = Camera::orbit_perspective(0.7, 0.3, 4.0, 0.9, 0.5, 0.2, 0.05, 200.0);
        let out = run_camera_lens(input_cam, &[], &[]);
        assert_eq!(out.pos, input_cam.pos);
        assert_eq!(out.fwd, input_cam.fwd);
        assert_eq!(out.right, input_cam.right);
        assert_eq!(out.up, input_cam.up);
        assert_eq!(out.near, input_cam.near);
        assert_eq!(out.far, input_cam.far);
        assert_eq!(out.mode, input_cam.mode);
        assert_eq!(out.view, input_cam.view);
    }

    #[test]
    fn params_flow_through_to_matching_lens_fields() {
        let input_cam = Camera::default_perspective();
        let out = run_camera_lens(
            input_cam,
            &[
                ("focus_distance", 12.5),
                ("f_stop", 2.8),
                ("shutter_angle", 180.0),
                ("exposure_ev", 1.5),
            ],
            &[],
        );
        assert_eq!(out.lens.focus_distance, 12.5);
        assert_eq!(out.lens.f_stop, 2.8);
        assert_eq!(out.lens.shutter_angle, 180.0);
        assert_eq!(out.lens.exposure_ev, 1.5);
    }

    #[test]
    fn wired_exposure_ev_overrides_its_same_named_param() {
        // Param says 1.0; the wire says 3.0 — the wire must win
        // (port-shadows-param, D4).
        let input_cam = Camera::default_perspective();
        let out = run_camera_lens(input_cam, &[("exposure_ev", 1.0)], &[("exposure_ev", 3.0)]);
        assert_eq!(out.lens.exposure_ev, 3.0, "wired exposure_ev should override the param");
        // Untouched siblings keep their param/default values.
        assert_eq!(out.lens.focus_distance, 0.0);
        assert_eq!(out.lens.shutter_angle, 0.0);
    }

    #[test]
    fn wired_f_stop_overrides_its_same_named_param() {
        let input_cam = Camera::default_perspective();
        let out = run_camera_lens(input_cam, &[("f_stop", 4.0)], &[("f_stop", 22.0)]);
        assert_eq!(out.lens.f_stop, 22.0, "wired f_stop should override the param");
    }
}
