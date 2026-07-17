//! `node.vignette` — soft fade-to-black border in three shapes.
//!
//! Two distinct use cases share one primitive:
//!   - **Cinematic**: `Circle` / `Ellipse` with `size ≈ 0.5..0.7` and a
//!     wide `softness` produces the classic vignetted edge darkening.
//!   - **Chain cleanup**: `Rectangle` with `size ≈ 0.98` and a tight
//!     `softness` hides the hard rectangular sampling cutoff that any
//!     UV-displacing op (feedback loops, mirror, kaleidoscope) produces
//!     at its boundary. The 3 feedback presets use this configuration.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Display labels for the `shape` enum. Index = enum value:
/// 0=Circle, 1=Ellipse, 2=Rectangle.
pub const VIGNETTE_SHAPES: &[&str] = &["Circle", "Ellipse", "Rectangle"];

crate::primitive! {
    name: Vignette,
    type_id: "node.vignette",
    purpose: "Soft fade-to-black border. Circle = aspect-corrected true circle (cinematic); Ellipse = canvas-fit oval; Rectangle = per-edge fade (hides hard sampling cutoffs in feedback / mirror / displacement chains). `size` sets the inner full-opacity boundary, `softness` is the fade width, `strength` blends the result back against the untouched input.",
    inputs: {
        in: Texture2D required,
        size: ScalarF32 optional,
        softness: ScalarF32 optional,
        strength: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("shape"),
            label: "Shape",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: VIGNETTE_SHAPES,
        },
        ParamDef {
            name: Cow::Borrowed("size"),
            label: "Size",
            ty: ParamType::Float,
            default: ParamValue::Float(0.7),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("softness"),
            label: "Softness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("strength"),
            label: "Strength",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "For cinematic vignettes use Circle (size≈0.5, softness≈0.5). For chain-cleanup edge fade (e.g. inside a feedback loop) use Rectangle (size≈0.98, softness≈0.04, strength=1.0). `size`/`softness`/`strength` are port-shadowed — wire any scalar producer to drive them live.",
    examples: ["preset.effect.stylized_feedback", "preset.effect.mandala"],
    picker: { label: "Vignette", category: Atom },
    summary: "Darkens the edges of the frame to pull the eye inward, with a circle, oval, or rectangular falloff. The cinematic edge fade.",
    category: Stylize,
    role: Filter,
    aliases: ["vignette", "edge fade", "border darken"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/vignette_body.wgsl"),
}

/// 16 bytes, matching the generated standalone kernel's `Params` (PARAMS order:
/// shape, size, softness, strength — no padding needed at 4 scalar fields).
/// `aspect` is gone: the body now derives it from `dims` (canvas size), so it
/// is no longer plumbed through the uniform.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VignetteUniforms {
    shape: u32,
    size: f32,
    softness: f32,
    strength: f32,
}

impl Primitive for Vignette {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let shape = match ctx.params.get("shape") {
            Some(ParamValue::Enum(v)) => (*v).min(2),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(2),
            _ => 0,
        };
        let size = match ctx.inputs.scalar("size") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("size") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.7,
            },
        };
        let softness = match ctx.inputs.scalar("softness") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("softness") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.3,
            },
        };
        let strength = match ctx.inputs.scalar("strength") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("strength") {
                Some(ParamValue::Float(f)) => *f,
                _ => 1.0,
            },
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);
        if width == 0 || height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: standalone kernel generated from the same
            // `wgsl_body` the fusion codegen chains — the first POSITIONAL atom,
            // so its body reads the ambient uv/dims the codegen threads in.
            // vignette.wgsl is retained as the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.vignette standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.vignette",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = VignetteUniforms {
            shape,
            size,
            softness,
            strength,
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
            "node.vignette",
        );
    }
}
