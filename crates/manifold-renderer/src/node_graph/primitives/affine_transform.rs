//! `node.transform` — pixel-exact replacement for legacy
//! Originally `TransformFX`. Fifth
//! §6.1 migration.
//!
//! 2D UV affine with aspect-correct rotation. The primitive surfaces
//! `rotation` in **degrees, screen-CW** (the user-facing convention
//! every DCC tool ships) — the deg→rad conversion + Y-down sign flip
//! happen inside `run()` before the uniform reaches the shader. This
//! keeps the V2 outer card and the per-node editor consistent: both
//! show the same degree value, neither surfaces a hidden conversion.
//! Math-style consumers that want radians can wrap the primitive in
//! their own preset graph and convert at *their* boundary.
//!
//! Distinct from the fold-mode `node.mirror` primitive (mirror/flip/
//! kaleidoscope-style folds, used by Mirror, QuadMirror, etc.). Both
//! operate on UV coordinates but their parameter surfaces and math
//! don't overlap; the AI surface lists both with composition_notes
//! calling out the difference.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use std::borrow::Cow;

crate::primitive! {
    name: AffineTransform,
    type_id: "node.transform",
    purpose: "2D UV affine: translate, scale, rotate around the center. Aspect-correct rotation; out-of-bounds samples return transparent black. Every affine param has a same-named scalar input port (port-shadows-param) — wire `translate_x`, `translate_y`, or `rotation` to drive the transform from a control producer (LFO, Color Compass, Math, …).",
    inputs: {
        in: Texture2D required,
        translate_x: ScalarF32 optional,
        translate_y: ScalarF32 optional,
        scale: ScalarF32 optional,
        rotation: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("translate_x"),
            label: "Translate X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("translate_y"),
            label: "Translate Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scale"),
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.1, 5.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rotation"),
            label: "Rotation",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-180.0, 180.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Warp,
    composition_notes: "1:1 building block for the legacy TransformFX effect. Rotation is in DEGREES, screen-CW (e.g. +90 rotates clockwise on screen) — the math conversion to radians + Y-down sign flip happens inside the primitive. Distinct from node.mirror (fold modes for Mirror); use this for affine, that for fold.",
    examples: ["preset.effect.transform"],
    picker: { label: "Transform", category: Atom },
    summary: "Moves, scales, and rotates the whole image around its centre. The basic transform for repositioning a layer.",
    category: DistortAndWarp,
    role: Filter,
    aliases: ["transform", "affine transform", "move scale rotate", "Transform TOP"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/affine_transform_body.wgsl"),
    input_access: [Gather],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AffineTransformUniforms {
    translate_x: f32,
    translate_y: f32,
    scale: f32,
    rotation: f32,
    aspect_ratio: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

impl Primitive for AffineTransform {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Wire-driven `translate_x` / `translate_y` shadow the params
        // when present — port-shadows-param. Same convention rotation
        // uses below; lets a control producer (Color Compass, LFO,
        // Math, …) drive the affine each frame.
        let translate_x = match ctx.inputs.scalar("translate_x") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("translate_x") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
        };
        let translate_y = match ctx.inputs.scalar("translate_y") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("translate_y") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
        };
        let scale = match ctx.inputs.scalar("scale") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("scale") {
                Some(ParamValue::Float(f)) => *f,
                _ => 1.0,
            },
        };
        // Read in user-facing units (degrees, screen-CW) and convert
        // to the shader's math frame (radians, math-CCW) inside the
        // primitive. Matches the legacy `TransformFX::apply` inline
        // conversion bit-for-bit. Wire-driven `rotation` shadows the
        // param when present — port-shadows-param convention.
        let rotation_degrees = match ctx.inputs.scalar("rotation") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("rotation") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
        };
        let rotation = -(rotation_degrees * std::f32::consts::PI / 180.0);

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);
        // Aspect is intrinsic to the texture, not a parameter — keeps
        // the primitive self-contained and matches the legacy value
        // (ctx.width / ctx.height) bit-for-bit when widths match.
        let aspect_ratio = width as f32 / height as f32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/affine_transform.wgsl"),
                "cs_main",
                "node.transform",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = AffineTransformUniforms {
            translate_x,
            translate_y,
            scale,
            rotation,
            aspect_ratio,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
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
                    texture: in_tex,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.transform",
        );
    }
}

#[cfg(test)]
mod port_shadow_tests {
    //! Sanity that the new `translate_x` / `translate_y` / `rotation`
    //! port-shadows are actually present after macro expansion and
    //! accept a wire from a scalar producer. If either of these
    //! fails the JSON loader silently drops the wire at runtime —
    //! which is the failure mode we want to catch here.

    use super::*;
    use crate::node_graph::effect_node::EffectNode;
    use crate::node_graph::graph::Graph;
    use crate::node_graph::primitives::Value;

    #[test]
    fn affine_transform_declares_all_three_scalar_input_ports() {
        let affine = AffineTransform::new();
        let port_names: Vec<_> = affine.inputs().iter().map(|p| p.name.as_ref()).collect();
        for needed in ["in", "translate_x", "translate_y", "rotation"] {
            assert!(
                port_names.contains(&needed),
                "missing port `{needed}` — actual ports = {port_names:?}",
            );
        }
    }

    #[test]
    fn can_connect_value_into_each_scalar_input_port() {
        for port in ["translate_x", "translate_y", "rotation"] {
            let mut g = Graph::new();
            let val = g.add_node(Box::new(Value::new()));
            let aff = g.add_node(Box::new(AffineTransform::new()));
            g.connect((val, "out"), (aff, port))
                .unwrap_or_else(|e| panic!("connect to AffineTransform.{port}: {e:?}"));
        }
    }
}
