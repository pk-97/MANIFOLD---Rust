//! `node.combine_xy` — zip two `Array<f32>` (x channel, y channel)
//! into a single `Array<CurvePoint>` that any line renderer can draw.
//!
//! The inverse of `node.split_xy` for the curve-rendering
//! pipeline. Takes axis-separated channels (built independently by
//! array_math / generate_range chains) and assembles them into the
//! typed `Array<CurvePoint>` wire that `node.draw_lines` consumes.
//!
//! Per-element math:
//!   `out[i].xy = vec2(x[i] * scale * PROJ_SCALE, y[i] * scale * PROJ_SCALE)`
//!
//! The `PROJ_SCALE = 0.25` screen-fit constant is baked inside the
//! primitive (per `DECOMPOSING_GENERATORS.md` §6.4 home #2 —
//! intrinsic to "produce curve-space points sized correctly for
//! render_lines"). The user-facing `scale` param (port-shadowed,
//! default 1.0) is the visible knob; the 0.25 lives where it
//! semantically belongs.
//!
//! CPU-only: small array sizes (curves ship at most a few hundred
//! points per frame), CPU is faster than the GPU dispatch overhead
//! and keeps downstream CPU readers (replicators, polyline stackers)
//! on the same-frame-coherent path.

use std::borrow::Cow;

use crate::generators::mesh_common::CurvePoint;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: PackCurveXy,
    type_id: "node.combine_xy",
    purpose: "Combine two Array<f32> (x channel, y channel) into one Array<CurvePoint>. The curve-pipeline counterpart to node.split_xy; the standard way to assemble a curve from independently-built axis chains (generate_range → array_math sweep → pack_curve_xy → render_lines). `scale` is port-shadows-param so an outer slider can rescale the whole curve at performance time. An internal PROJ_SCALE = 0.25 screen-fit constant is folded into the output: at scale = 1.0 the curve fills the inner 50% of the screen — matches the legacy generator_math::PROJ_SCALE convention so existing line-renderer presets stay visually identical. CPU-only — runs on the content thread so downstream CPU consumers see same-frame writes.",
    inputs: {
        x: Array(f32) required,
        y: Array(f32) required,
        scale: ScalarF32 optional,
    },
    outputs: {
        out: Array(CurvePoint),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("scale"),
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Output capacity follows the `x` input (mirrors how node.array_math sizes its `out` to its `a` input) so the pack auto-sizes to whatever the upstream curve length is — no separate `max_capacity` knob to keep in sync. Processing truncates to min(x_capacity, y_capacity, out_capacity) so a shorter axis channel naturally clips the curve length. The internal PROJ_SCALE = 0.25 is intentionally not a user param — it's the screen-fit factor baked into the curve-space contract render_lines expects. Pair with node.range + node.array_math (ScaleOffset + Sin) per axis to build any parametric curve (Lissajous, Rose, hypocycloid, audio waveform). To cancel the PROJ_SCALE factor (e.g. when the upstream axis chain already encodes the desired screen-fractional radius), set `scale = 4.0` — this is the documented pattern for polygon outlines whose `size` param is already in screen-fractional units.",
    examples: [],
    picker: { label: "Combine XY (curve)", category: Atom },
    summary: "Zips two number lists, X and Y, into one list of points ready to draw as a line or curve.",
    category: Geometry3D,
    role: Filter,
    aliases: ["combine xy", "pack curve xy", "pack curve", "zip points"],
    boundary_reason: NonGpu,
}

impl PackCurveXy {
    /// Screen-fit factor folded into every emitted CurvePoint. The legacy
    /// `LissajousGenerator` baked the same 0.25 via `generator_math::
    /// PROJ_SCALE`; preserving the constant inside this primitive keeps
    /// every existing line-renderer preset visually identical when their
    /// curve source switches over to a decomposed graph (per the
    /// "decomposition must not be a visual regression" parity bar).
    pub const PROJ_SCALE: f32 = 0.25;
}

impl Primitive for PackCurveXy {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        // Output is one-to-one with the x channel. run() truncates the
        // processing to min(x, y, out) at frame time, so a shorter y
        // input naturally clips the curve length.
        input_capacities
            .iter()
            .find(|(p, _)| *p == "x")
            .map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let scale = ctx.scalar_or_param("scale", 1.0);

        let Some(x_buf) = ctx.inputs.array("x") else {
            return;
        };
        let Some(y_buf) = ctx.inputs.array("y") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            log::warn!(
                "node.combine_xy: no GpuBuffer bound to output port `out` — \
                 the chain build did not pre-allocate this Array<CurvePoint>. \
                 Confirm `max_capacity` is set on this node.",
            );
            return;
        };

        let f32_size = std::mem::size_of::<f32>() as u64;
        let pt_size = std::mem::size_of::<CurvePoint>() as u64;
        let x_capacity = (x_buf.size / f32_size) as u32;
        let y_capacity = (y_buf.size / f32_size) as u32;
        let out_capacity = (out_buf.size / pt_size) as u32;
        let count = x_capacity.min(y_capacity).min(out_capacity);
        if count == 0 {
            return;
        }

        // ── Read inputs via mapped_ptr ──
        let x_ptr = x_buf
            .mapped_ptr()
            .expect("pack_curve_xy: `x` input must be shared-memory");
        let y_ptr = y_buf
            .mapped_ptr()
            .expect("pack_curve_xy: `y` input must be shared-memory");
        let x_slice: &[f32] =
            unsafe { std::slice::from_raw_parts(x_ptr as *const f32, x_capacity as usize) };
        let y_slice: &[f32] =
            unsafe { std::slice::from_raw_parts(y_ptr as *const f32, y_capacity as usize) };

        // ── Pack into stack scratch then bulk-write ──
        const SCRATCH_LEN: usize = 4096;
        let mut scratch = [CurvePoint { xy: [0.0, 0.0] }; SCRATCH_LEN];
        let write_count = (count as usize).min(SCRATCH_LEN);
        let k = scale * Self::PROJ_SCALE;
        for (i, slot) in scratch.iter_mut().take(write_count).enumerate() {
            *slot = CurvePoint {
                xy: [x_slice[i] * k, y_slice[i] * k],
            };
        }

        // Safety: shared-memory MTLBuffer pre-bound by the chain build;
        // write count clamped to the buffer capacity above; sequential
        // executor on the content thread means no concurrent writer.
        unsafe {
            out_buf.write(0, bytemuck::cast_slice(&scratch[..write_count]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_x_y_required_scale_optional_and_curvepoint_output() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};

        assert_eq!(PackCurveXy::TYPE_ID, "node.combine_xy");

        let f32_layout = ArrayType::of_known::<f32>();
        let curve_layout = ArrayType::of_known::<CurvePoint>();

        let x_in = PackCurveXy::INPUTS.iter().find(|p| p.name == "x").unwrap();
        let y_in = PackCurveXy::INPUTS.iter().find(|p| p.name == "y").unwrap();
        let scale_in = PackCurveXy::INPUTS
            .iter()
            .find(|p| p.name == "scale")
            .unwrap();
        assert!(x_in.required);
        assert!(y_in.required);
        assert!(!scale_in.required);
        assert_eq!(x_in.ty, PortType::Array(f32_layout));
        assert_eq!(y_in.ty, PortType::Array(f32_layout));
        assert_eq!(scale_in.ty, PortType::Scalar(ScalarType::F32));

        assert_eq!(PackCurveXy::OUTPUTS.len(), 1);
        assert_eq!(PackCurveXy::OUTPUTS[0].name, "out");
        assert_eq!(PackCurveXy::OUTPUTS[0].ty, PortType::Array(curve_layout));
    }

    #[test]
    fn output_capacity_follows_x_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = PackCurveXy::new();
        let params = ParamValues::default();
        let inputs = [("x", 256_u32), ("y", 256_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(256),
            "pack auto-sizes to the x channel capacity",
        );
        let mismatched = [("x", 128_u32), ("y", 64_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &mismatched),
            Some(128),
            "output sizes to x; run() truncates to min(x, y, out) at processing time",
        );
    }

    #[test]
    fn proj_scale_is_quarter_to_match_legacy_lissajous() {
        // Locks the parity contract: the legacy LissajousGenerator
        // baked generator_math::PROJ_SCALE = 0.25 into every emitted
        // vertex. Decomposed Lissajous routes through pack_curve_xy,
        // which is now the single home for that constant — drifting
        // it silently shrinks/grows every line-renderer preset.
        assert_eq!(PackCurveXy::PROJ_SCALE, 0.25);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = PackCurveXy::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.combine_xy");
    }
}
