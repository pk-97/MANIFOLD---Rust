//! `node.metallic_glass_process` — fused 4-stage MetallicGlass
//! process pass. Bit-exact wrap of
//! `generators/shaders/metallic_glass_process.wgsl` via include_str.
//!
//! Each output texel:
//!   - HEIGHT (.r): unmirrored feedback luma → levels-remapped
//!   - METALLIC (.g): mirrored Sobel-edge → levels-remapped
//!   - EDGE (.b): raw Sobel magnitude (for downstream PBR vein detail)
//!   - ALPHA (.a): 1.0
//! Temporal-blended with `prev_tex` (default blend 0.15 = stable surface).
//!
//! Fused for MetallicGlass parity. Splitting Sobel / mirror /
//! levels / temporal-blend into atomic primitives would round
//! through fp16 intermediates and break parity.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProcessUniforms {
    edge_strength: f32,
    mirror_angle: f32,
    width: f32,
    height: f32,
    temporal_blend: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: MetallicGlassProcess,
    type_id: "node.metallic_glass_process",
    purpose: "Fused MetallicGlass process pass: Sobel edge detection on mirrored UV + height-map levels + metallic-map levels + temporal blend with previous frame. Writes packed (height, metallic, edge, 1.0) to an Rgba16Float texture. Fused for parity — splitting introduces fp16 quantization that breaks bit-exact match to the legacy MetallicGlass.",
    inputs: {
        feedback: Texture2D required,
        prev: Texture2D required,
    },
    outputs: {
        processed: Texture2D,
    },
    params: [
        ParamDef {
            name: "edge_strength",
            label: "Edge Strength",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 8.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "mirror_angle",
            label: "Mirror Angle",
            ty: ParamType::Float,
            default: ParamValue::Float(0.785398),
            range: Some((-6.28318, 6.28318)),
            enum_values: &[],
        },
        ParamDef {
            name: "temporal_blend",
            label: "Temporal Blend",
            ty: ParamType::Float,
            default: ParamValue::Float(0.15),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Inputs: feedback (current frame's blurred feedback luma source) and prev (last frame's processed output for temporal stability). For MetallicGlass JSON decomposition: pair upstream with the noise+blur feedback pipeline, route `prev` through node.feedback (or array_feedback if we add a Texture2D version). temporal_blend=0.15 matches the default; 1.0 disables blending (jittery), 0.0 freezes the surface.",
    examples: [],
    picker: { label: "Metallic Glass Process", category: Atom },
}

impl Primitive for MetallicGlassProcess {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let edge_strength = match ctx.params.get("edge_strength") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let mirror_angle = match ctx.params.get("mirror_angle") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.785398,
        };
        let temporal_blend = match ctx.params.get("temporal_blend") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.15,
        };

        let Some(feedback) = ctx.inputs.texture_2d("feedback") else {
            return;
        };
        let Some(prev) = ctx.inputs.texture_2d("prev") else {
            return;
        };
        let Some(processed) = ctx.outputs.texture_2d("processed") else {
            return;
        };
        let width = processed.width;
        let height = processed.height;
        if width == 0 || height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("../../generators/shaders/metallic_glass_process.wgsl"),
                "cs_main",
                "node.metallic_glass_process",
            )
        });

        let uniforms = ProcessUniforms {
            edge_strength,
            mirror_angle,
            width: width as f32,
            height: height as f32,
            temporal_blend,
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
                    texture: feedback,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: processed,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: prev,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.metallic_glass_process",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn metallic_glass_process_declares_two_texture_inputs_and_one_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(MetallicGlassProcess::TYPE_ID, "node.metallic_glass_process");
        assert_eq!(MetallicGlassProcess::INPUTS.len(), 2);
        assert_eq!(MetallicGlassProcess::INPUTS[0].name, "feedback");
        assert_eq!(MetallicGlassProcess::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(MetallicGlassProcess::INPUTS[1].name, "prev");
        assert_eq!(MetallicGlassProcess::INPUTS[1].ty, PortType::Texture2D);
        assert_eq!(MetallicGlassProcess::OUTPUTS.len(), 1);
        assert_eq!(MetallicGlassProcess::OUTPUTS[0].name, "processed");
        assert_eq!(MetallicGlassProcess::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn metallic_glass_process_has_edge_mirror_temporal_params() {
        let names: Vec<&str> = MetallicGlassProcess::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["edge_strength", "mirror_angle", "temporal_blend"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = MetallicGlassProcess::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.metallic_glass_process");
    }
}
