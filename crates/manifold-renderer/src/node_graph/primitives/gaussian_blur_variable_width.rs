//! `node.gaussian_blur_variable_width` — per-pixel-width separable
//! Gaussian blur. One dispatch = one axis; pair two with ping-pong
//! for a full 2D blur.
//!
//! Adapted from `effects/shaders/fx_depth_of_field_compute.wgsl`'s
//! 9-/17-/25-tap blur kernels, with the width source decoupled from
//! input.alpha (separate Texture2D input now) so the primitive
//! composes with any width source — DoF's CoC pass, a procedural
//! mask, a depth-gradient texture, etc.
//!
//! Two specialization knobs flatten dead branches at pipeline
//! creation time:
//!
//!   * `quality`        — Low (9-tap) / Medium (17-tap) / High (25-tap).
//!   * `weighting_mode` — None (plain Gaussian) / ScatterAsGatherByCoC
//!                        (foreground-bleed guard for CoC-driven blurs).
//!
//! The 6 specialized pipelines are lazily compiled on first use and
//! cached. Defaults preserve the original behaviour (Medium + None).

use ahash::AHashMap;
use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const BLUR_VARIABLE_AXES: &[&str] = &["Horizontal", "Vertical"];
pub const BLUR_VARIABLE_QUALITIES: &[&str] = &["Low", "Medium", "High"];
pub const BLUR_VARIABLE_WEIGHTINGS: &[&str] = &["None", "ScatterAsGatherByCoC"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurUniforms {
    direction: u32,
    max_radius: f32,
    _pad0: u32,
    _pad1: u32,
}

const BLUR_WGSL: &str =
    include_str!("shaders/gaussian_blur_variable_width.wgsl");

crate::primitive! {
    name: GaussianBlurVariableWidth,
    type_id: "node.gaussian_blur_variable_width",
    purpose: "Separable Gaussian blur where the per-pixel kernel width is sampled from a `width` Texture2D's R channel. One dispatch handles one axis (horizontal or vertical); pair two with ping-pong textures for a 2D blur. Three quality levels (9-/17-/25-tap kernels at σ≈2/4/6) and an optional CoC-driven scatter-as-gather weighting that prevents sharp pixels bleeding into blurry regions — load-bearing for DoF-class effects.",
    inputs: {
        in: Texture2D required,
        width: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "axis",
            label: "Axis",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: BLUR_VARIABLE_AXES,
        },
        ParamDef {
            name: "max_radius",
            label: "Max Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(12.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "quality",
            label: "Quality",
            ty: ParamType::Enum,
            default: ParamValue::Enum(1),
            range: None,
            enum_values: BLUR_VARIABLE_QUALITIES,
        },
        ParamDef {
            name: "weighting_mode",
            label: "Weighting Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: BLUR_VARIABLE_WEIGHTINGS,
        },
    ],
    composition_notes: "step_size = width_sample × max_radius + 1.0 along the chosen axis. width_sample < 0.005 produces a pass-through (in-focus). For a full 2D blur: dispatch this primitive twice with axis=Horizontal then axis=Vertical, ping-ponging between two Rgba16Float textures. ScatterAsGatherByCoC: each neighbor only contributes if its CoC (sampled from the `width` texture's R channel) ≥ the center pixel's CoC, OR the center is itself very blurry (CoC > 0.5). For DoF parity set max_radius = 6.0 and weighting_mode = ScatterAsGatherByCoC; the kernel matches the legacy DoF blur byte-for-byte.",
    examples: [],
    picker: { label: "Gaussian Blur (Variable Width)", category: Atom },
    extra_fields: {
        pipelines: AHashMap<u32, GpuComputePipeline> = AHashMap::new(),
    },
}

fn read_enum(ctx: &EffectNodeContext<'_, '_>, name: &str, default: u32) -> u32 {
    match ctx.params.get(name) {
        Some(ParamValue::Enum(n)) => *n,
        Some(ParamValue::Float(f)) => f.round() as u32,
        _ => default,
    }
}

fn pipeline_key(quality: u32, weighting: u32) -> u32 {
    weighting * 3 + quality
}

impl Primitive for GaussianBlurVariableWidth {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let direction = read_enum(ctx, "axis", 0);
        let max_radius = match ctx.params.get("max_radius") {
            Some(ParamValue::Float(f)) => *f,
            _ => 12.0,
        };
        let quality = read_enum(ctx, "quality", 1).min(2);
        let weighting = read_enum(ctx, "weighting_mode", 0).min(1);

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(width_tex) = ctx.inputs.texture_2d("width") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let w = target.width;
        let h = target.height;
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let key = pipeline_key(quality, weighting);
        let pipeline = self
            .pipelines
            .entry(key)
            .or_insert_with(|| {
                let quality_str = match quality {
                    0 => "0u",
                    2 => "2u",
                    _ => "1u",
                };
                let weighting_str = if weighting == 1 { "1u" } else { "0u" };
                let label = format!(
                    "node.gaussian_blur_variable_width.q{quality}.w{weighting}"
                );
                gpu.device.create_specialized_compute_pipeline(
                    BLUR_WGSL,
                    "cs_main",
                    &[
                        ("QUALITY_LEVEL", quality_str),
                        ("WEIGHTING_MODE", weighting_str),
                    ],
                    &label,
                )
            });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = BlurUniforms {
            direction,
            max_radius,
            _pad0: 0,
            _pad1: 0,
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
                    texture: src,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: width_tex,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.gaussian_blur_variable_width",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn gaussian_blur_variable_width_declares_two_texture_inputs_and_one_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(
            GaussianBlurVariableWidth::TYPE_ID,
            "node.gaussian_blur_variable_width"
        );
        assert_eq!(GaussianBlurVariableWidth::INPUTS.len(), 2);
        assert_eq!(GaussianBlurVariableWidth::INPUTS[0].name, "in");
        assert_eq!(GaussianBlurVariableWidth::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(GaussianBlurVariableWidth::INPUTS[1].name, "width");
        assert_eq!(GaussianBlurVariableWidth::INPUTS[1].ty, PortType::Texture2D);
        assert_eq!(GaussianBlurVariableWidth::OUTPUTS.len(), 1);
        assert_eq!(GaussianBlurVariableWidth::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn gaussian_blur_variable_width_has_axis_radius_quality_weighting_params() {
        let names: Vec<&str> = GaussianBlurVariableWidth::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec!["axis", "max_radius", "quality", "weighting_mode"]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GaussianBlurVariableWidth::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.gaussian_blur_variable_width");
    }
}
