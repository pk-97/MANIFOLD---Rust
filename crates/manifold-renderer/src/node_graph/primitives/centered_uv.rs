//! `node.centered_uv` — UV recentered around (cx, cy) with per-axis scale.
//!
//! `out.r = (uv.x - cx) * scale_x`,
//! `out.g = (uv.y - cy) * scale_y`.
//!
//! The canonical "screen-centered, aspect-corrected" coordinate space
//! for any procedural pattern that wants to compose around a chosen
//! origin. Defaults (cx = cy = 0.5) preserve the legacy screen-centered
//! behaviour; override for off-center procedurals, focus-pulled SDFs,
//! and any pattern that needs to follow a moving anchor.
//!
//! Replaces the value+math chain that an explicit
//! `(uv - center) * (aspect, 1) * inverse_scale` decomposition would
//! otherwise need — every centered procedural reads from this one
//! primitive and slices out the channels it wants via `field_combine`,
//! `distance_to_point`, etc.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CenteredUvUniforms {
    cx: f32,
    cy: f32,
    scale_x: f32,
    scale_y: f32,
}

crate::primitive! {
    name: CenteredUv,
    type_id: "node.centered_uv",
    purpose: "UV recentered around (cx, cy) with per-axis scale. out.r = (uv.x - cx) * scale_x, out.g = (uv.y - cy) * scale_y. The canonical centered/aspect-corrected coordinate space for procedural patterns — replaces the explicit (uv - center) * (aspect, 1) * inverse_scale chain a centered field would otherwise need. Defaults cx = cy = 0.5 preserve screen-centered behaviour.",
    inputs: {
        // All four params port-shadowable so the typical Plasma-style
        // composition (`scale_x = aspect * inverse_scale`,
        // `scale_y = inverse_scale`) can be driven from upstream
        // Math nodes each frame, and cx / cy can follow an animated
        // anchor (a face-tracker centroid, a focus-pull driver, etc.).
        cx: ScalarF32 optional,
        cy: ScalarF32 optional,
        scale_x: ScalarF32 optional,
        scale_y: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
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
            name: Cow::Borrowed("scale_x"),
            label: "Scale X",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-32.0, 32.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scale_y"),
            label: "Scale Y",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-32.0, 32.0)),
            enum_values: &[],
        },
    ],
    // depth_rule: zero-input UV-coordinate producer (identical shape to a generator) but its output is a coordinate lookup table for a downstream node.remap/texture_advect, not visual content — no depth origin of its own, Terminal per the ambiguous-source default
    depth_rule: Terminal,
    composition_notes: "Pairs naturally with node.field_combine to slice X / Y / X+Y projections out of the centered space (a=1 b=0 for X, a=0 b=1 for Y, a=1 b=1 for X+Y), and with node.distance_to_point (cx=0 cy=0) for the radial projection. For aspect-correct patterns, wire `scale_x` from a Math node that multiplies aspect by an inverse-scale. Override cx / cy to recenter procedurals on any point — e.g. a centered SDF that follows a face-tracker centroid.",
    examples: [],
    picker: { label: "Centered UV", category: Atom },
    summary: "Outputs each pixel's position measured from a centre point, so the middle reads zero and the edges spread out. The base for radial and zoom effects.",
    category: FieldsAndCoordinates,
    role: Source,
    aliases: ["centered uv", "centered coordinates", "radial"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/centered_uv_body.wgsl"),
}

impl Primitive for CenteredUv {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cx = ctx.scalar_or_param("cx", 0.5);
        let cy = ctx.scalar_or_param("cy", 0.5);
        let scale_x = ctx.scalar_or_param("scale_x", 1.0);
        let scale_y = ctx.scalar_or_param("scale_y", 1.0);

        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.centered_uv standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.centered_uv",
            )
        });

        let uniforms = CenteredUvUniforms {
            cx,
            cy,
            scale_x,
            scale_y,
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
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.centered_uv",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn centered_uv_declares_four_optional_scalar_inputs_and_one_texture_output() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(CenteredUv::TYPE_ID, "node.centered_uv");
        let ins = CenteredUv::INPUTS;
        assert_eq!(ins.len(), 4);
        let names: Vec<&str> = ins.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["cx", "cy", "scale_x", "scale_y"]);
        for port in ins {
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(CenteredUv::OUTPUTS.len(), 1);
        assert_eq!(CenteredUv::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn centered_uv_has_cx_cy_scale_x_scale_y_params() {
        let names: Vec<&str> = CenteredUv::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["cx", "cy", "scale_x", "scale_y"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = CenteredUv::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.centered_uv");
    }
}
