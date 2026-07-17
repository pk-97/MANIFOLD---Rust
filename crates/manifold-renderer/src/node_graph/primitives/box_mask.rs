//! `node.rectangle_mask` — rotated rectangular SDF mask (Chebyshev distance).
//!
//! Pure generator. Output: single-channel mask in RGB (A = 1) with
//! value 1.0 inside the box, 0.0 outside, smoothstepped over a
//! `softness`-controlled falloff band. All six geometric params are
//! port-shadowable for per-frame modulation.
//!
//! `half_width` / `half_height` are the box's half-extents from the
//! center — same "extent from center" semantics as `ellipse_mask`'s
//! `radius_x` / `radius_y`. To make a band that spans the canvas in
//! one axis (DoF tilt-shift, scanline, letterbox), set that axis'
//! half-extent past 0.5 (e.g. 1.0 ≥ any UV pixel's center-distance).
//! Combined with `rotation`, this produces arbitrary rotated bands.
//!
//! Pairs with `node.masked_mix`, `node.variable_blur.width`,
//! or any other mask consumer. For DoF tilt-shift flip polarity with
//! `node.invert` downstream (CoC wants outside=1, mask atom convention
//! is inside=1).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BoxMaskUniforms {
    cx: f32,
    cy: f32,
    half_width: f32,
    half_height: f32,
    rotation: f32,
    softness: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: BoxMask,
    type_id: "node.rectangle_mask",
    purpose: "Rotated rectangular SDF mask (Chebyshev distance). Output: RGB = mask value (inside=1, outside=0, smoothstep falloff of width `softness`), A = 1. half_width / half_height are extents from the center (same convention as ellipse_mask's radii). For canvas-spanning bands, set the unbounded axis' half-extent ≥ 1.0; combined with rotation that gives rotated band masks for tilt-shift, scanlines, and letterboxes.",
    inputs: {
        cx: ScalarF32 optional,
        cy: ScalarF32 optional,
        half_width: ScalarF32 optional,
        half_height: ScalarF32 optional,
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
            name: Cow::Borrowed("half_width"),
            label: "Half Width",
            ty: ParamType::Float,
            default: ParamValue::Float(0.25),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("half_height"),
            label: "Half Height",
            ty: ParamType::Float,
            default: ParamValue::Float(0.25),
            range: Some((0.0, 4.0)),
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
            default: ParamValue::Float(0.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Mask value = 1 - smoothstep(1 - softness, 1 + softness, max(|x_local|/half_width, |y_local|/half_height)). softness is in normalized half-extent units — softness=0 is a hard edge; softness=1 means the falloff extends a full half-extent beyond the nominal edge. For DoF tilt-shift: cx=0.5, cy=focus_y, half_width=1.0 (spans canvas), half_height=focus_width, softness=0.5, rotation=tilt_angle → pipe through node.invert to get the standard CoC.",
    examples: [],
    picker: { label: "Rectangle Mask", category: Atom },
    summary: "Draws a soft-edged rectangle you can use to limit an effect to one region of the frame. Position it, size it, rotate it, and soften the edge.",
    category: Mask,
    role: Source,
    aliases: ["rectangle mask", "box mask", "rect", "Rectangle TOP"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/box_mask_body.wgsl"),
}

impl Primitive for BoxMask {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cx = ctx.scalar_or_param("cx", 0.5);
        let cy = ctx.scalar_or_param("cy", 0.5);
        let half_width = ctx.scalar_or_param("half_width", 0.25);
        let half_height = ctx.scalar_or_param("half_height", 0.25);
        let rotation = ctx.scalar_or_param("rotation", 0.0);
        let softness = ctx.scalar_or_param("softness", 0.0);

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
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.rectangle_mask standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.rectangle_mask",
            )
        });

        let uniforms = BoxMaskUniforms {
            cx,
            cy,
            half_width,
            half_height,
            rotation,
            softness,
            _pad0: 0.0,
            _pad1: 0.0,
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
            "node.rectangle_mask",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn box_mask_declares_six_optional_scalar_inputs_and_one_texture_output() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(BoxMask::TYPE_ID, "node.rectangle_mask");
        let ins = BoxMask::INPUTS;
        assert_eq!(ins.len(), 6);
        let names: Vec<&str> = ins.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["cx", "cy", "half_width", "half_height", "rotation", "softness"]
        );
        for port in ins {
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(BoxMask::OUTPUTS.len(), 1);
        assert_eq!(BoxMask::OUTPUTS[0].name, "out");
        assert!(BoxMask::OUTPUTS[0].ty.is_texture_2d());
    }

    #[test]
    fn box_mask_has_all_six_params() {
        let names: Vec<&str> = BoxMask::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["cx", "cy", "half_width", "half_height", "rotation", "softness"]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = BoxMask::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.rectangle_mask");
    }
}
