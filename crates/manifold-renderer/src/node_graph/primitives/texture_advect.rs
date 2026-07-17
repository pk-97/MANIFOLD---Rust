//! `node.texture_advect` — backward (semi-Lagrangian) advection of a
//! texture by a 2D velocity field.
//!
//! For each output pixel: read the velocity at this UV, sample the
//! source at `uv − velocity * dt / dims`, write that sample. The minus
//! sign is the standard fluid advection convention ("look back along
//! the velocity to find where this pixel came from").
//!
//! `dt` has units of pixels-per-frame and is resolution-independent —
//! the shader divides by dims internally so a `dt = 1.0` knob means
//! "displace by `velocity` pixels each frame" at any resolution.

use std::borrow::Cow;
use manifold_gpu::{GpuAddressMode, GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const TEXTURE_ADVECT_BOUNDARIES: &[&str] = &["Repeat", "Clamp"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AdvectUniforms {
    dt: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: TextureAdvect,
    type_id: "node.texture_advect",
    purpose: "Backward (semi-Lagrangian) advection of a texture by a 2D velocity field. For each output pixel: sample the source at `uv - velocity.rg * dt / dims`. The universal fluid op — used for color advection by velocity, self-advection of velocity by itself, smoke / dye / paint transport. `dt` is in pixels-per-frame and resolution-independent.",
    inputs: {
        in: Texture2D required,
        velocity: Texture2D required,
        dt: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("dt"),
            label: "Δt (pixels/frame)",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("boundary"),
            label: "Boundary",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: TEXTURE_ADVECT_BOUNDARIES,
        },
    ],
    depth_rule: Warp,
    composition_notes: "Velocity is read from the RG channels of `velocity`; BA ignored. Output samples preserve all RGBA channels of `in`. Use Repeat for toroidal fluid sims (the oily-fluid family); Clamp when off-canvas samples should fade to edge color. For self-advection (advect velocity by itself), wire the same velocity to BOTH `in` and `velocity`. Negative `dt` runs the advection backwards (useful for trail-erase effects). The `dt` port shadows the param so an LFO can pulse the flow.",
    examples: [],
    picker: { label: "Texture Advect", category: Atom },
    summary: "Drags a texture along a velocity field, carrying the pixels with the flow. The transport step in a fluid simulation.",
    category: FieldsAndCoordinates,
    role: Filter,
    aliases: ["advect", "transport", "flow", "fluid"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/texture_advect_body.wgsl"),
    input_access: [Gather, Coincident],
    extra_fields: {
        sampler_repeat: Option<manifold_gpu::GpuSampler> = None,
        sampler_clamp: Option<manifold_gpu::GpuSampler> = None,
    },
}

impl Primitive for TextureAdvect {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let dt = match ctx.inputs.scalar("dt") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("dt") {
                Some(ParamValue::Float(f)) => *f,
                _ => 1.0,
            },
        };
        let boundary = match ctx.params.get("boundary") {
            Some(ParamValue::Enum(v)) => (*v).min(1),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(1),
            _ => 0,
        };

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(vel) = ctx.inputs.texture_2d("velocity") else {
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
            // `in` is a Gather input (sampled at the advected UV), `velocity` is
            // coincident. Generated kernel binds uniform(0)/in(1)/velocity(2)/
            // samp(3)/dst(4); the body ignores the `boundary` param (the sampler
            // below carries the wrap mode). texture_advect.wgsl is the oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.texture_advect standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.texture_advect",
            )
        });
        let sampler_repeat = self.sampler_repeat.get_or_insert_with(|| {
            gpu.device.create_sampler(&GpuSamplerDesc {
                address_mode_u: GpuAddressMode::Repeat,
                address_mode_v: GpuAddressMode::Repeat,
                address_mode_w: GpuAddressMode::Repeat,
                ..Default::default()
            })
        });
        let sampler_clamp = self
            .sampler_clamp
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        let sampler = if boundary == 0 {
            sampler_repeat
        } else {
            sampler_clamp
        };

        let uniforms = AdvectUniforms {
            dt,
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
                GpuBinding::Texture {
                    binding: 2,
                    texture: vel,
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
            "node.texture_advect",
        );
    }
}
