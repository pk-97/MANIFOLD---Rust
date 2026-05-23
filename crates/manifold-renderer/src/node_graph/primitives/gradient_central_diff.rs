//! `node.gradient_central_diff` — per-pixel central-difference
//! gradient of one channel of an input texture.
//!
//! Outputs (dx, dy, 0, 1) in RGBA, where dx = (right − left) / 2 and
//! dy = (up − down) / 2 for the chosen channel. The standard vec2
//! gradient used by Sobel-light edge detectors, fluid-sim curl
//! extraction, height-to-normal pipelines, and any per-pixel
//! finite-difference math.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const GRADIENT_CHANNELS: &[&str] = &["R", "G", "B", "A"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GradientUniforms {
    channel: u32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: GradientCentralDiff,
    type_id: "node.gradient_central_diff",
    purpose: "Per-pixel central-difference gradient of a single input channel. Output: (dx, dy, 0, 1) in RGBA — dx and dy are the half-difference of neighbouring samples along x and y. The standard vec2 gradient atom: feeds Sobel edge detectors, fluid-sim curl-from-color extraction, heightmap→normal pipelines (for the tangent part), reaction-diffusion flow seeding.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "channel",
            label: "Channel",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: GRADIENT_CHANNELS,
        },
    ],
    composition_notes: "Output is a SIGNED vec2 field. Pair with `node.normalize_vec2` for direction-only gradients (used in fluid-sim curl forcing), or feed directly into `node.rotate_vec2_90` for curl-perpendicular flow. For a per-channel gradient of an RG texture (oily-fluid pattern), instance this primitive twice with channel=R and channel=G and combine downstream.",
    examples: [],
    picker: { label: "Gradient (Central Diff)", category: Atom },
}

impl Primitive for GradientCentralDiff {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let channel = match ctx.params.get("channel") {
            Some(ParamValue::Enum(v)) => (*v).min(3),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(3),
            _ => 0,
        };

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (target.width, target.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/gradient_central_diff.wgsl"),
                "cs_main",
                "node.gradient_central_diff",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = GradientUniforms {
            channel,
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
                    texture: src,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.gradient_central_diff",
        );
    }
}
