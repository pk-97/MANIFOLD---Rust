//! `node.circle_mask` — rotated elliptical SDF mask in UV space.
//!
//! Pure generator. Output is a single-channel mask in RGB (A = 1)
//! with value 1.0 inside the ellipse, 0.0 outside, smoothstepped
//! over a `softness`-controlled falloff band. All five geometric
//! params are port-shadowable for per-frame modulation.
//!
//! Industry-standard "elliptical mask with feather" — same shape as
//! Photoshop / After Effects / DaVinci marquee tools. Pairs with
//! `node.masked_mix`, `node.variable_blur.width`, or
//! any other mask consumer. For radial focus effects flip polarity
//! with `node.invert` downstream (DoF wants outside=1, mask atom
//! convention is inside=1).
//!
//! No internal aspect correction. Operates in raw UV [0, 1]² space —
//! a circle (radius_x = radius_y) will visually stretch on a wide
//! canvas. Wire `node.texture_size.aspect → math(Divide) →
//! radius_x` if you want a true circle regardless of canvas shape.

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EllipseMaskUniforms {
    cx: f32,
    cy: f32,
    radius_x: f32,
    radius_y: f32,
    rotation: f32,
    softness: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: EllipseMask,
    type_id: "node.circle_mask",
    purpose: "Rotated elliptical SDF mask. Output: RGB = mask value (inside=1, outside=0, smoothstep falloff of width `softness`), A = 1. Pure UV-space; no canvas-aspect correction. Industry-standard masking convention — pairs with masked_mix downstream, and with node.invert when the polarity needs flipping (e.g. DoF wants outside=1).",
    inputs: {
        cx: ScalarF32 optional,
        cy: ScalarF32 optional,
        radius_x: ScalarF32 optional,
        radius_y: ScalarF32 optional,
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
            name: Cow::Borrowed("radius_x"),
            label: "Radius X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.25),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("radius_y"),
            label: "Radius Y",
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
    composition_notes: "Mask value = 1 - smoothstep(1 - softness, 1 + softness, length(rotated_uv / (rx, ry))). softness is in normalized-radius units — softness=0 is a hard edge, softness=1 means the falloff extends a full radius beyond the nominal edge. Radius=0 collapses the axis to a point (the mask never reaches 1 along that axis); useful for line-like masks. For DoF radial: cx=focus_x, cy=focus_y, radius_x=radius_y=focus_width, softness=0.5 → pipe through node.invert to get the standard CoC (outside=1, inside=0). For aspect-correct circles: wire aspect from node.texture_size and divide one radius by it.",
    examples: [],
    picker: { label: "Circle Mask", category: Atom },
    summary: "Draws a soft-edged circle to limit an effect to a round region. It can stretch into an oval and rotate.",
    category: Mask,
    role: Source,
    aliases: ["circle mask", "ellipse mask", "oval", "Circle TOP"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/ellipse_mask_body.wgsl"),
}

impl Primitive for EllipseMask {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cx = ctx.scalar_or_param("cx", 0.5);
        let cy = ctx.scalar_or_param("cy", 0.5);
        let radius_x = ctx.scalar_or_param("radius_x", 0.25);
        let radius_y = ctx.scalar_or_param("radius_y", 0.25);
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
                    .expect("node.circle_mask standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.circle_mask",
            )
        });

        let uniforms = EllipseMaskUniforms {
            cx,
            cy,
            radius_x,
            radius_y,
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
            "node.circle_mask",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn ellipse_mask_declares_six_optional_scalar_inputs_and_one_texture_output() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(EllipseMask::TYPE_ID, "node.circle_mask");
        let ins = EllipseMask::INPUTS;
        assert_eq!(ins.len(), 6);
        let names: Vec<&str> = ins.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["cx", "cy", "radius_x", "radius_y", "rotation", "softness"]
        );
        for port in ins {
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(EllipseMask::OUTPUTS.len(), 1);
        assert_eq!(EllipseMask::OUTPUTS[0].name, "out");
        assert!(EllipseMask::OUTPUTS[0].ty.is_texture_2d());
    }

    #[test]
    fn ellipse_mask_has_all_six_params() {
        let names: Vec<&str> = EllipseMask::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["cx", "cy", "radius_x", "radius_y", "rotation", "softness"]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = EllipseMask::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.circle_mask");
    }
}
