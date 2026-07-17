//! `node.linear_gradient` — directional 0→1 ramp in UV space.
//!
//! Pure generator. Output: single-channel ramp in RGB (A = 1) with
//! value 0 on the "negative" side of the rotated axis, 1 on the
//! "positive" side, smoothstepped across a band of width `softness`
//! centred at (cx, cy). All four geometric params port-shadowable.
//!
//! Industry-standard "linear gradient with feather" — same shape as
//! TouchDesigner's Ramp TOP in linear mode, or Photoshop's gradient
//! tool with foreground→background. Pairs with `node.masked_mix`,
//! `node.compose` blend factors, fades, wipes, and any "intensity ramps
//! along this direction" usage.

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LinearGradientUniforms {
    cx: f32,
    cy: f32,
    rotation: f32,
    softness: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
}

crate::primitive! {
    name: LinearGradient,
    type_id: "node.linear_gradient",
    purpose: "Directional 0→1 ramp in UV space. Output: RGB = ramp value (0 on the negative side of the rotated axis, 1 on the positive side, smoothstep transition of width `softness` centred at (cx, cy)), A = 1. The straight-line gradient — pairs with masked_mix for fades / wipes, with node.invert to flip direction, or with a 1D LUT (node.lut1d) to remap the ramp into arbitrary value curves.",
    inputs: {
        cx: ScalarF32 optional,
        cy: ScalarF32 optional,
        rotation: ScalarF32 optional,
        softness: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D[R: MASK, G: MASK, B: MASK, A: VALID],
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("cx"),
            label: "Center X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((-1.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("cy"),
            label: "Center Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((-1.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rotation"),
            label: "Rotation",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("softness"),
            label: "Softness",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "mask = smoothstep(-softness/2, +softness/2, dot(uv - center, (cos rotation, sin rotation))). softness ≈ 0 is a hard step at the center line; softness = 1 ≈ a half-canvas transition; softness ≥ 1.5 spans the canvas diagonal so the ramp never saturates. rotation=0 ramps left→right; rotation=π/2 ramps bottom→top. Cheap polarity flip: wire through node.invert (or just set rotation += π).",
    examples: [],
    picker: { label: "Linear Gradient", category: Atom },
    summary: "A straight light-to-dark ramp across the frame at any angle. The simplest gradient, good for fades, masks, and ramps to drive other effects.",
    category: Generate,
    role: Source,
    aliases: ["linear gradient", "ramp", "fade", "Ramp TOP", "Gradient Texture"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/linear_gradient_body.wgsl"),
}

impl Primitive for LinearGradient {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cx = ctx.scalar_or_param("cx", 0.5);
        let cy = ctx.scalar_or_param("cy", 0.5);
        let rotation = ctx.scalar_or_param("rotation", 0.0);
        let softness = ctx.scalar_or_param("softness", 1.0);

        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let w = target.width;
        let h = target.height;
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.linear_gradient standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.linear_gradient",
            )
        });

        let uniforms = LinearGradientUniforms {
            cx,
            cy,
            rotation,
            softness,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
            _pad3: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.linear_gradient",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn linear_gradient_declares_four_optional_scalar_inputs_and_one_texture_output() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(LinearGradient::TYPE_ID, "node.linear_gradient");
        let ins = LinearGradient::INPUTS;
        assert_eq!(ins.len(), 4);
        let names: Vec<&str> = ins.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["cx", "cy", "rotation", "softness"]);
        for port in ins {
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(LinearGradient::OUTPUTS.len(), 1);
        assert_eq!(LinearGradient::OUTPUTS[0].name, "out");
        assert!(LinearGradient::OUTPUTS[0].ty.is_texture_2d());
    }

    #[test]
    fn linear_gradient_has_all_four_params() {
        let names: Vec<&str> = LinearGradient::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["cx", "cy", "rotation", "softness"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = LinearGradient::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.linear_gradient");
    }
}
