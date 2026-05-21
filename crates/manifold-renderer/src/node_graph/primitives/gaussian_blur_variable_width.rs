//! `node.gaussian_blur_variable_width` — per-pixel-width separable
//! Gaussian blur. One dispatch = one axis; pair two with ping-pong
//! for a full 2D blur.
//!
//! Adapted from `effects/shaders/fx_depth_of_field_compute.wgsl`'s
//! 17-tap blur, with the width source decoupled from input.alpha
//! (separate Texture2D input now) so the primitive composes with
//! any width source — DoF's CoC pass, a procedural mask, a depth-
//! gradient texture, etc.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const BLUR_VARIABLE_AXES: &[&str] = &["Horizontal", "Vertical"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurUniforms {
    direction: u32,
    max_radius: f32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: GaussianBlurVariableWidth,
    type_id: "node.gaussian_blur_variable_width",
    purpose: "Separable Gaussian blur where the per-pixel kernel width is sampled from a `width` Texture2D's R channel. One dispatch handles one axis (horizontal or vertical); pair two with ping-pong textures for a 2D blur. Used by DoF (CoC-driven blur), depth-of-field-style effects, and any compositional where blur radius varies by spatial mask.",
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
    ],
    composition_notes: "17-tap kernel (sigma ≈ 4.0). step_size = width_sample × max_radius + 1.0, applied along the chosen axis. width_sample < 0.005 produces a pass-through (in-focus). For a full 2D blur: dispatch this primitive twice with axis=Horizontal then axis=Vertical, ping-ponging between two Rgba16Float textures.",
    examples: [],
    picker: { label: "Gaussian Blur (Variable Width)", category: Atom },
}

impl Primitive for GaussianBlurVariableWidth {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let direction = match ctx.params.get("axis") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 0,
        };
        let max_radius = match ctx.params.get("max_radius") {
            Some(ParamValue::Float(f)) => *f,
            _ => 12.0,
        };

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
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/gaussian_blur_variable_width.wgsl"),
                "cs_main",
                "node.gaussian_blur_variable_width",
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
    fn gaussian_blur_variable_width_has_axis_and_max_radius_params() {
        let names: Vec<&str> = GaussianBlurVariableWidth::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["axis", "max_radius"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GaussianBlurVariableWidth::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.gaussian_blur_variable_width");
    }
}
