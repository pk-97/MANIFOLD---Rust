//! `node.rotate_vector` — rotate the RG vec2 field by an
//! arbitrary angle (radians).
//!
//! Generalisation of the older `node.rotate_vec2_90` (retired; a load-time
//! migration folds it into this primitive with `angle` seeded to PI/2,
//! docs/NODE_VOCABULARY_AUDIT.md §7.1). `angle` is port-shadow-param so a control wire — LFO, audio
//! envelope, clip-trigger driver, manual slider — can sweep the
//! rotation continuously. Defaults to `angle = PI/2` so existing
//! presets that simply replaced `rotate_vec2_90` with this primitive
//! get the +90° CCW behaviour for free.
//!
//! The standard atom for curl-from-gradient force fields: rotating a
//! density gradient by 90° gives perpendicular flow (divergence-free
//! curl); rotating by a continuous angle gives the FluidSim2D-style
//! "rotation_angle" sweep used to bias the flow off-axis.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

// Standalone-codegen uniform layout: the single `angle` param (the body computes
// cos/sin itself, where the hand uniform carried CPU-precomputed cos_a/sin_a).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RotateUniforms {
    angle: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: RotateVec2ByAngle,
    type_id: "node.rotate_vector",
    purpose: "Rotate the input's RG vec2 field by an arbitrary angle (radians) per pixel. `out.x = v.x*cos - v.y*sin`, `out.y = v.x*sin + v.y*cos`. The general curl-from-gradient atom — defaults to angle = PI/2 (+90° CCW, the divergence-free curl-flow case) but the angle is port-shadow-param so a control wire (LFO, driver, manual slider, clip-trigger envelope) can sweep it continuously. Sweeping the angle is how FluidSim2D's `rotation_angle` knob biases the flow off the pure-curl axis.",
    inputs: {
        in: Texture2D required,
        angle: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("angle"),
            label: "Angle",
            ty: ParamType::Angle,
            default: ParamValue::Float(std::f32::consts::FRAC_PI_2),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
    ],
    // depth_rule: rotates the per-pixel vector VALUE stored in RG (pointwise), not the sampling UV — distinct from node.transform/rotate_coordinates which rotate the image itself
    depth_rule: Inherit,
    composition_notes: "BA of the input are ignored; output BA = (0, 1). Chain order for fluid-sim curl: `gradient_central_diff(scale_mode=UV, wrap_mode=Repeat) → scale_offset_texture(slope_strength * area_scale) → rotate_vec2_by_angle(angle)` — the decomposed shape of the legacy `fluid_gradient_rotate` bundle. For the oily-fluid divergence-free curl pattern, leave angle at the default PI/2 and wire a normalized gradient into `in`. For larger rotations of a UV-space transform use `node.rotate_coordinates` (different operation — that's a UV-space coordinate transform, not a per-pixel vec2 rotation). The retired `node.rotate_vec2_90` type-ID folds into this primitive at load time with `angle` seeded to PI/2 (docs/NODE_VOCABULARY_AUDIT.md §7.1); saved projects keep working unchanged.",
    examples: [],
    picker: { label: "Rotate Vector", category: Atom },
    summary: "Rotates a 2D vector field by an angle, turning every arrow in a flow or gradient field by the same amount.",
    category: FieldsAndCoordinates,
    role: Map,
    aliases: ["rotate vector", "rotate vec2 by angle", "turn", "rotate flow"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/rotate_vec2_by_angle_body.wgsl"),
    extra_fields: {
        // fp32-output opt-in (see gradient_central_diff): full-precision
        // intermediate/output inside a feedback loop so fused == unfused.
        output_format_override: Option<manifold_gpu::GpuTextureFormat> = None,
    },
}

impl Primitive for RotateVec2ByAngle {
    fn output_format(&self, port: &str) -> Option<manifold_gpu::GpuTextureFormat> {
        if port == "out" {
            self.output_format_override
        } else {
            None
        }
    }

    fn set_output_format(&mut self, port: &str, format: manifold_gpu::GpuTextureFormat) {
        if port == "out" {
            self.output_format_override = Some(format);
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let angle = ctx.scalar_or_param("angle", std::f32::consts::FRAC_PI_2);

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
        let out_fmt = self
            .output_format_override
            .unwrap_or(manifold_gpu::GpuTextureFormat::Rgba16Float);
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Pointwise. Generated kernel binds uniform(0)/tex(1)/samp(2)/dst(3);
            // the body computes cos/sin from `angle`. rotate_vec2_by_angle.wgsl is
            // the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec_fmt::<Self>(out_fmt)
                    .expect("node.rotate_vector standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.rotate_vector",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = RotateUniforms {
            angle,
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
            "node.rotate_vector",
        );
    }
}
