//! `node.camera_switch` — two-input camera mux (SCENE_MODIFIER_FRAMEWORK_DESIGN
//! section 3.5).
//!
//! Selects between two `Camera` inputs on `select` (enum "A"/"B"): A passes
//! `a` through, B passes `b`. An unwired input falls back to the other, so a
//! lone producer on either side still reaches `out`. The enable wiring of
//! camera-path scene modifiers (D5 Switch decl) mints one of these between
//! the previous camera producer and the lens — toggling the modifier is a
//! param write on `select`, never a graph rebuild.
//!
//! CPU-only — `Camera` is a value type drained per wire per frame, same
//! convention as `node.loop_camera`. Non-GPU: outside the freeze-codegen
//! mandate (ADDING_PRIMITIVES.md's CPU-atom exclusion).

use std::borrow::Cow;

use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const CAMERA_SWITCH_SELECT_LABELS: &[&str] = &["A", "B"];

crate::primitive! {
    name: CameraSwitch,
    type_id: "node.camera_switch",
    purpose: "Select between two Camera inputs with an enum select: A passes `a`, B passes `b`. An unwired input falls back to the other, so a single producer on either side still reaches `out`. Camera-path scene modifiers mint one between the previous camera producer and the lens — the modifier's enable toggle is a param write on `select` (D5 Switch), never a structural edit. CPU-only; Camera is a value type (like loop_camera's outputs).",
    inputs: {
        a: Camera optional,
        b: Camera optional,
    },
    outputs: {
        out: Camera,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("select"),
            label: "Select",
            ty: ParamType::Enum,
            // Standalone default A = pass `a` through. A modifier apply
            // stamps B (its own camera is wired to `b`, enabled by default).
            default: ParamValue::Enum(0),
            range: None,
            enum_values: CAMERA_SWITCH_SELECT_LABELS,
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Minted by camera-path scene modifier applies (SCENE_MODIFIER_FRAMEWORK D5 Switch), never authored by hand in v1: previous camera producer → `a`, the modifier's camera → `b`, `out` → the repointed camera port. Toggle = one param write on `select`; disabled (A) seamlessly restores the original camera with zero structural churn. Unwired input falls back to the other, so the mux degrades to a pass-through if one side never got wired.",
    examples: [],
    picker: { label: "Camera Switch", category: Driver },
    summary: "Switches between two cameras. Scene modifiers use it so toggling the modifier on and off never rebuilds the graph.",
    category: Geometry3D,
    role: Source,
    aliases: ["camera switch", "camera mux", "mux camera"],
    boundary_reason: NonGpu,
}

impl Primitive for CameraSwitch {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let select_b = matches!(
            ctx.params.get("select"),
            Some(ParamValue::Enum(n)) if *n == 1
        );
        let a = ctx.inputs.camera("a");
        let b = ctx.inputs.camera("b");
        // Unwired input falls back to the other; nothing wired at all falls
        // back to the identity-ish default (the same fallback consumers use).
        let cam = match (select_b, a, b) {
            (false, Some(cam), _) => cam,
            (false, None, Some(cam)) => cam,
            (true, _, Some(cam)) => cam,
            (true, Some(cam), None) => cam,
            (false, None, None) | (true, None, None) => Camera::default_perspective(),
        };
        ctx.outputs.set_camera("out", cam);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn camera_switch_declares_two_camera_inputs_and_one_camera_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(CameraSwitch::TYPE_ID, "node.camera_switch");
        let in_names: Vec<&str> = CameraSwitch::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(in_names, vec!["a", "b"]);
        for input in CameraSwitch::INPUTS {
            assert!(!input.required, "{} should be optional", input.name);
            assert_eq!(input.ty, PortType::Camera);
        }
        assert_eq!(CameraSwitch::OUTPUTS.len(), 1);
        assert_eq!(CameraSwitch::OUTPUTS[0].name, "out");
        assert_eq!(CameraSwitch::OUTPUTS[0].ty, PortType::Camera);
    }

    #[test]
    fn camera_switch_has_one_enum_select_param() {
        assert_eq!(CameraSwitch::PARAMS.len(), 1);
        assert_eq!(CameraSwitch::PARAMS[0].name, "select");
        assert_eq!(CameraSwitch::PARAMS[0].enum_values, &["A", "B"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = CameraSwitch::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.camera_switch");
    }
}
