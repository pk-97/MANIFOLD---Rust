//! `node.canvas_area_scale` — `(width * height) / reference_area` as a
//! scalar wire. The "splat density brightness compensation" primitive.
//!
//! Splat-based density displays (FluidSim2D, StrangeAttractor, BlackHole,
//! ParticleText) all share the same calibration: as output resolution
//! grows, each particle lights a smaller fraction of the canvas, so
//! display intensity has to scale with `width × height` to keep
//! brightness constant. Hardcoded inside legacy display shaders today;
//! exposed here as a reusable scalar source so any particle graph can
//! wire it into its tone-map's `intensity` (typically through one
//! `node.math(Multiply, base_intensity)` step).
//!
//! `reference_area` defaults to `1920 × 1080 = 2_073_600` to match the
//! legacy generators' baseline. Override per-preset if a different
//! reference makes more sense for your calibration.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: CanvasAreaScale,
    type_id: "node.canvas_area_scale",
    purpose: "Emit (width * height) / reference_area as a scalar. The brightness compensation atom for splat-based density displays — wire `width` from `system.generator_input.output_width` and `height` from `system.generator_input.output_height`, then multiply the result into a tone-map's `intensity` so the canvas stays equally bright across output resolutions.",
    inputs: {
        width: ScalarF32 optional,
        height: ScalarF32 optional,
    },
    outputs: {
        out: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("width"),
            label: "Width",
            ty: ParamType::Float,
            default: ParamValue::Float(1920.0),
            range: Some((1.0, 16384.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("height"),
            label: "Height",
            ty: ParamType::Float,
            default: ParamValue::Float(1080.0),
            range: Some((1.0, 16384.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("reference_area"),
            label: "Reference Area",
            ty: ParamType::Float,
            default: ParamValue::Float(2_073_600.0),
            range: Some((1.0, 1.0e9)),
            enum_values: &[],
        },
        // Minimum dimensions clamp the input width/height upward
        // before computing the area ratio. Mirrors the legacy
        // FluidSimCore's scatter-resolution clamp
        // (`max(640, output_width * field_scale)`): below the
        // minimum canvas size, the splat density would otherwise
        // shrink linearly with area and dim the output below
        // perceptual usefulness on small windows (perform-mode HUD,
        // external monitor). Default 0 = no clamp (existing
        // behaviour, no regression for non-fluid consumers).
        ParamDef {
            name: Cow::Borrowed("min_width"),
            label: "Min Width",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 16384.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("min_height"),
            label: "Min Height",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 16384.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Pair with `system.generator_input.output_width` and `output_height` to compute the runtime canvas-area ratio against the configured `reference_area` (default 1920×1080 = 2,073,600). Use the output to scale a tone-map's `intensity` so splat-based density displays stay perceptually consistent across resolutions. `reference_area = 0` falls back to 1.0 (passthrough) to avoid div-by-zero on misconfigured presets. `min_width` / `min_height` clamp the inputs upward — set to 640 / 360 for FluidSim2D parity so small windows don't dim below usable brightness.",
    examples: [],
    picker: { label: "Canvas Area Scale", category: Driver },
    summary: "Outputs how big the canvas is compared to a reference size, used to keep particle brightness steady when the resolution changes.",
    category: Control,
    role: Control,
    aliases: ["canvas area scale", "resolution compensation", "area"],
    boundary_reason: NonGpu,
}

impl Primitive for CanvasAreaScale {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let width = ctx.scalar_or_param("width", 1920.0);
        let height = ctx.scalar_or_param("height", 1080.0);
        let reference_area = ctx.scalar_or_param("reference_area", 2_073_600.0);
        let min_w = match ctx.params.get("min_width") {
            Some(ParamValue::Float(f)) => f.max(0.0),
            _ => 0.0,
        };
        let min_h = match ctx.params.get("min_height") {
            Some(ParamValue::Float(f)) => f.max(0.0),
            _ => 0.0,
        };
        let effective_w = width.max(min_w);
        let effective_h = height.max(min_h);
        let out = if reference_area > 0.0 {
            (effective_w * effective_h) / reference_area
        } else {
            1.0
        };
        ctx.outputs.set_scalar("out", ParamValue::Float(out));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn canvas_area_scale_declares_two_scalar_inputs_and_one_scalar_output() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(CanvasAreaScale::TYPE_ID, "node.canvas_area_scale");
        let ins = CanvasAreaScale::INPUTS;
        assert_eq!(ins.len(), 2);
        assert_eq!(ins[0].name, "width");
        assert_eq!(ins[0].ty, PortType::Scalar(ScalarType::F32));
        assert!(!ins[0].required);
        assert_eq!(ins[1].name, "height");
        assert_eq!(ins[1].ty, PortType::Scalar(ScalarType::F32));
        assert!(!ins[1].required);
        assert_eq!(CanvasAreaScale::OUTPUTS.len(), 1);
        assert_eq!(
            CanvasAreaScale::OUTPUTS[0].ty,
            PortType::Scalar(ScalarType::F32)
        );
    }

    #[test]
    fn canvas_area_scale_has_width_height_reference_area_and_min_params() {
        let names: Vec<&str> = CanvasAreaScale::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["width", "height", "reference_area", "min_width", "min_height"]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = CanvasAreaScale::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.canvas_area_scale");
    }

    /// Default-params output is `1.0` for 1920×1080 against the
    /// same reference area. This is what a graph sees if the user
    /// hasn't wired the boundary node yet — sensible passthrough,
    /// not a div-by-zero or zero-brightness surprise.
    #[test]
    fn default_params_evaluate_to_unity() {
        use crate::node_graph::{Graph, compile, validate, Executor, FrameTime};
        use manifold_core::{Beats, Seconds};

        let mut g = Graph::new();
        let _ = g.add_node(Box::new(CanvasAreaScale::new()));
        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        let time = FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };
        exec.execute_frame(&mut g, &plan, time);
    }

    /// Mathematical correctness — 3840×2160 against 1920×1080 reference
    /// yields exactly 4.0 (4x the pixel count).
    #[test]
    fn four_k_against_full_hd_reference_is_four() {
        let w = 3840.0_f32;
        let h = 2160.0_f32;
        let ref_a = 1920.0_f32 * 1080.0_f32;
        assert_eq!((w * h) / ref_a, 4.0);
    }

    /// `reference_area = 0` is treated as the unity passthrough fallback
    /// (not a division by zero). This protects misconfigured presets
    /// from silently dimming everything to NaN/Inf.
    #[test]
    fn zero_reference_area_falls_back_to_one() {
        let w = 1920.0_f32;
        let h = 1080.0_f32;
        let ref_a = 0.0_f32;
        let out = if ref_a > 0.0 { (w * h) / ref_a } else { 1.0 };
        assert_eq!(out, 1.0);
    }
}
