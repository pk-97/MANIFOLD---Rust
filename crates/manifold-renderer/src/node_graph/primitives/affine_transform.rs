//! `node.affine_transform` — pixel-exact replacement for legacy
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
//! Distinct from the existing fold-mode `Transform` primitive (used
//! by Mirror, QuadMirror, etc.). Both operate on UV coordinates but
//! their parameter surfaces and math don't overlap; the AI surface
//! lists both with composition_notes calling out the difference.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: AffineTransform,
    type_id: "node.affine_transform",
    purpose: "2D UV affine: translate, scale, rotate around the center. Aspect-correct rotation; out-of-bounds samples return transparent black. The `rotation` input port is the standard control-wire shadow of the `rotation` param — wire any scalar source (LFO, Color Compass, Math, …) to spin the image at runtime.",
    inputs: {
        in: Texture2D required,
        rotation: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "translate_x",
            label: "Translate X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "translate_y",
            label: "Translate Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "scale",
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.1, 5.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "rotation",
            label: "Rotation",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-180.0, 180.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "1:1 building block for the legacy Transform effect. Rotation is in DEGREES, screen-CW (e.g. +90 rotates clockwise on screen) — the math conversion to radians + Y-down sign flip happens inside the primitive. Distinct from Transform (fold modes for Mirror); use this for affine, that for fold.",
    examples: ["preset.effect.transform"],
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
        let translate_x = match ctx.params.get("translate_x") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let translate_y = match ctx.params.get("translate_y") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let scale = match ctx.params.get("scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
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
                "node.affine_transform",
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
            "node.affine_transform",
        );
    }
}
