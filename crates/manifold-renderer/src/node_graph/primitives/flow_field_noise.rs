//! `node.flow_field_noise` — domain-warped fBM Perlin noise →
//! 2D flow vector field in a Texture2D.
//!
//! Pure generator (zero inputs). Output writes `flow_x` into the R
//! channel and `flow_y` into the B channel (G=0, A=1.0) — matches
//! the Watercolor mode-2 packing convention so downstream UV
//! displacement primitives that read `.rb` as a flow vector
//! compose cleanly. Animated over `time` (slow evolution).

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FlowFieldUniforms {
    time: f32,
    z_scale: f32,
    warp_scale: f32,
    _pad0: f32,
}

crate::primitive! {
    name: FlowFieldNoise,
    type_id: "node.flow_field_noise",
    purpose: "Generate a 2D flow vector field from domain-warped fBM Perlin noise. Zero inputs. Output: Rgba16Float texture with flow_x in R, flow_y in B (G=0, A=1.0) — matches the Watercolor flow-map convention so it composes with UV displacement primitives that read .rb as offset.",
    inputs: {},
    outputs: {
        flow: Texture2D,
    },
    params: [
        ParamDef {
            name: "z_scale",
            label: "Time Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(0.01),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "warp_scale",
            label: "Domain Warp",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Output values are roughly in [-1, 1] (raw fBM range). Pair with a UV displacement primitive that scales by a `displace_weight` (Watercolor uses 0.001). z_scale = 0.01 gives Watercolor's default slow evolution; raise for faster animation. warp_scale = 0.5 matches Watercolor.",
    examples: [],
    picker: { label: "Flow Field Noise", category: Atom },
}

impl Primitive for FlowFieldNoise {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let z_scale = match ctx.params.get("z_scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.01,
        };
        let warp_scale = match ctx.params.get("warp_scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        };
        let time = ctx.time.seconds.0 as f32;

        let Some(flow) = ctx.outputs.texture_2d("flow") else {
            return;
        };
        let w = flow.width;
        let h = flow.height;
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/flow_field_noise.wgsl"),
                "cs_main",
                "node.flow_field_noise",
            )
        });

        let uniforms = FlowFieldUniforms {
            time,
            z_scale,
            warp_scale,
            _pad0: 0.0,
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
                    texture: flow,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.flow_field_noise",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn flow_field_noise_declares_zero_inputs_and_texture_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(FlowFieldNoise::TYPE_ID, "node.flow_field_noise");
        assert!(FlowFieldNoise::INPUTS.is_empty());
        assert_eq!(FlowFieldNoise::OUTPUTS.len(), 1);
        assert_eq!(FlowFieldNoise::OUTPUTS[0].name, "flow");
        assert_eq!(FlowFieldNoise::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn flow_field_noise_has_z_scale_and_warp_scale_params() {
        let names: Vec<&str> = FlowFieldNoise::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["z_scale", "warp_scale"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = FlowFieldNoise::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.flow_field_noise");
    }
}
