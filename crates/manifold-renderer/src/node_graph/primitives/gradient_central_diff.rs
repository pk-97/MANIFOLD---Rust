//! `node.gradient_central_diff` — per-pixel central-difference
//! gradient of one channel of an input texture.
//!
//! Outputs (dx, dy, 0, 1) in RGBA, where dx = (right − left) / 2 and
//! dy = (up − down) / 2 for the chosen channel. The standard vec2
//! gradient used by Sobel-light edge detectors, fluid-sim curl
//! extraction, height-to-normal pipelines, and any per-pixel
//! finite-difference math.

use manifold_gpu::{GpuAddressMode, GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const GRADIENT_CHANNELS: &[&str] = &["R", "G", "B", "A"];

/// Output scaling. `Texel`: dx = (R - L) * 0.5 (default — texel-space
/// finite difference, matches the legacy oily-fluid / heightmap-to-normal
/// consumers). `UV`: dx = (R - L) * W * 0.5, dy = (U - D) * H * 0.5 —
/// per-axis multiplication by the dimension halves so the output is in
/// per-UV-unit space, what fluid-sim gradient-rotate consumers need.
pub const GRADIENT_SCALE_MODES: &[&str] = &["Texel", "UV"];

/// Boundary policy. `Clamp` (default): bilinear sampling with the default
/// clamp-to-edge sampler — matches existing behaviour, suitable for
/// non-cyclic textures (heightmaps, oily-fluid normals). `Repeat`: the
/// neighbour taps wrap toroidally via a repeat sampler, required for
/// fluid sims whose density field is cyclic.
pub const GRADIENT_WRAP_MODES: &[&str] = &["Clamp", "Repeat"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GradientUniforms {
    channel: u32,
    scale_mode: u32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: GradientCentralDiff,
    type_id: "node.gradient_central_diff",
    purpose: "Per-pixel central-difference gradient of a single input channel. Output: (dx, dy, 0, 1) in RGBA. `scale_mode` selects Texel-space (`(R - L) * 0.5` — default, matches oily-fluid / heightmap-to-normal usage) or UV-space (`(R - L) * W * 0.5` per-axis — multiplies by the dimension halves so output is in per-UV-unit space, what fluid-sim gradient-rotate needs). `wrap_mode` selects Clamp (default, bilinear-clamp sampler) or Repeat (toroidal sampler for cyclic fluid sims). The standard vec2 gradient atom: feeds Sobel edge detectors, fluid-sim curl-from-color extraction, heightmap→normal pipelines, reaction-diffusion flow seeding.",
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
        ParamDef {
            name: "scale_mode",
            label: "Scale Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: GRADIENT_SCALE_MODES,
        },
        ParamDef {
            name: "wrap_mode",
            label: "Wrap Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: GRADIENT_WRAP_MODES,
        },
    ],
    composition_notes: "Output is a SIGNED vec2 field. Pair with `node.normalize_vec2` for direction-only gradients (used in fluid-sim curl forcing), or feed directly into `node.rotate_vec2_by_angle` for arbitrary-angle curl flow. For a per-channel gradient of an RG texture (oily-fluid pattern), instance this primitive twice with channel=R and channel=G and combine downstream. Defaults (Texel + Clamp) preserve legacy oily-fluid / heightmap behaviour. Use scale_mode=UV + wrap_mode=Repeat to compose with `scale_offset_texture` + `rotate_vec2_by_angle` as the decomposed `fluid_gradient_rotate` pipeline.",
    examples: [],
    picker: { label: "Gradient (Central Diff)", category: Atom },
    extra_fields: {
        repeat_sampler: Option<manifold_gpu::GpuSampler> = None,
    },
}

impl Primitive for GradientCentralDiff {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let channel = match ctx.params.get("channel") {
            Some(ParamValue::Enum(v)) => (*v).min(3),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(3),
            _ => 0,
        };
        let scale_mode = match ctx.params.get("scale_mode") {
            Some(ParamValue::Enum(v)) => (*v).min(1),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(1),
            _ => 0,
        };
        let wrap_repeat = match ctx.params.get("wrap_mode") {
            Some(ParamValue::Enum(v)) => *v == 1,
            Some(ParamValue::Float(f)) => f.round() as u32 == 1,
            _ => false,
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
        let clamp_sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        let repeat_sampler = self.repeat_sampler.get_or_insert_with(|| {
            gpu.device.create_sampler(&GpuSamplerDesc {
                address_mode_u: GpuAddressMode::Repeat,
                address_mode_v: GpuAddressMode::Repeat,
                address_mode_w: GpuAddressMode::Repeat,
                ..Default::default()
            })
        });
        let sampler = if wrap_repeat {
            repeat_sampler
        } else {
            clamp_sampler
        };

        let uniforms = GradientUniforms {
            channel,
            scale_mode,
            _pad0: 0.0,
            _pad1: 0.0,
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
