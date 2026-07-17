//! `node.flow_field_noise` — domain-warped fBM Perlin noise →
//! 2D flow vector field in a Texture2D.
//!
//! Pure generator (zero inputs). Output writes `flow_x` into the R
//! channel and `flow_y` into the B channel (G=0, A=1.0) — matches
//! the Watercolor mode-2 packing convention so downstream UV
//! displacement primitives that read `.rb` as a flow vector
//! compose cleanly. Animated over `time` (slow evolution).

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Output-resolution options. The flow field is low-frequency, so it
/// tolerates being generated at reduced resolution and sampled back
/// bilinearly — which is exactly what the original Watercolor effect
/// did. Generating fBM is the expensive part (it scales with pixel
/// count), so half- or quarter-res cuts the cost 4× / 16×.
pub const FLOW_RESOLUTIONS: &[&str] = &["full", "half", "quarter"];

/// Decode the `resolution` enum into a `(num, denom)` canvas scale, or
/// `None` for full-res (canvas-default).
fn resolution_scale(params: &crate::node_graph::effect_node::ParamValues) -> Option<(u32, u32)> {
    let idx = match params.get("resolution") {
        Some(ParamValue::Enum(n)) => *n,
        Some(ParamValue::Float(f)) => f.round() as u32,
        _ => 0,
    };
    match idx {
        1 => Some((1, 2)), // half
        2 => Some((1, 4)), // quarter
        _ => None,         // full
    }
}

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
        // Backing param for time (run() packs FrameTime.seconds). First so the
        // generated uniform layout matches the hand {time, z_scale, warp_scale}.
        ParamDef {
            name: Cow::Borrowed("time"),
            label: "Time",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1e9)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("z_scale"),
            label: "Time Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(0.01),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("warp_scale"),
            label: "Domain Warp",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("resolution"),
            label: "Resolution",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 2.0)),
            enum_values: FLOW_RESOLUTIONS,
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Output values are roughly in [-1, 1] (raw fBM range). Pair with a UV displacement primitive that scales by a `displace_weight` (Watercolor uses 0.001). z_scale = 0.01 gives Watercolor's default slow evolution; raise for faster animation. warp_scale = 0 skips the two domain-warp fBM evaluations entirely (the cheap direct-eval flow the original Watercolor used); raise it for swirlier flow. resolution = half/quarter generates the field at reduced resolution (4× / 16× cheaper) — the field is low-frequency so downstream bilinear sampling upscales it cleanly; full-res is the default.",
    examples: [],
    picker: { label: "Flow Field Noise", category: Atom },
    summary: "Generates a swirling 2D flow field from layered noise, the velocity field you feed into advect or displace for fluid-like motion.",
    category: Noise,
    role: Source,
    aliases: ["flow field", "noise flow", "velocity", "curl"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/flow_field_noise_body.wgsl"),
}

impl Primitive for FlowFieldNoise {
    fn output_canvas_scale(
        &self,
        port: &str,
        params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        if port != "flow" {
            return None;
        }
        // Zero-input generator: the executor has no input dims to size
        // the slot from, so it consults this directly (the propagation
        // path is skipped when there are no wired inputs). Declaring
        // canvas/2 or canvas/4 lands the flow slot at reduced size; the
        // shader derives its uv from `textureDimensions(output_tex)` so
        // it renders correctly at any size, and `uv_displace_by_flow`
        // samples it bilinearly — free upscale for a low-freq field.
        resolution_scale(params)
    }

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
            // Source generator: uniform(0)/dst(1). The body reads time/z_scale/
            // warp_scale; the `resolution` param (output-size control, handled
            // Rust-side) maps to the uniform's pad slot and is ignored by the body.
            // flow_field_noise.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.flow_field_noise standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
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
    fn flow_field_noise_has_z_scale_warp_scale_and_resolution_params() {
        // `time` leads the list: the freeze fusion regularized the frame-time
        // input as a port-shadowed `time` param (the time-param pattern) so the
        // generated kernel can read it as a uniform field.
        let names: Vec<&str> = FlowFieldNoise::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["time", "z_scale", "warp_scale", "resolution"]);
    }

    #[test]
    fn resolution_param_drives_output_canvas_scale() {
        use crate::node_graph::EffectNode;
        let prim = FlowFieldNoise::new();
        let node: &dyn EffectNode = &prim;
        for (enum_v, expected) in [
            (0u32, None),
            (1, Some((1u32, 2u32))),
            (2, Some((1u32, 4u32))),
        ] {
            let mut params = ahash::AHashMap::default();
            params.insert(std::borrow::Cow::Borrowed("resolution"), ParamValue::Enum(enum_v));
            assert_eq!(
                node.output_canvas_scale("flow", &params),
                expected,
                "resolution enum {enum_v}",
            );
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = FlowFieldNoise::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.flow_field_noise");
    }
}
