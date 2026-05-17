//! `node.edge_stretch` — pixel-exact replacement for legacy
//! [`EdgeStretchFX`](crate::effects::edge_stretch::EdgeStretchFX).
//! Third §6.1 migration.
//!
//! Clamps UV coordinates to a center strip (width-controlled,
//! axis-dependent), stretching the edge pixels outward. Used as
//! `EdgeStretch` in the editor today; renamed `clamp_stretch` in the
//! primitive library because the operation is more general than its
//! single existing application (e.g., a VoronoiPrism preset can
//! repurpose the same primitive).

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: ClampStretch,
    type_id: "node.edge_stretch",
    purpose: "Clamps UV coordinates to a center strip (width × axis), stretching edge pixels outward. Axis modes: Horizontal, Vertical, or Both.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "amount",
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "source_width",
            label: "Source Width",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.1, 0.9)),
            enum_values: &[],
        },
        ParamDef {
            name: "direction",
            label: "Direction",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 2.0)),
            enum_values: &["Horiz", "Vert", "Both"],
        },
    ],
    composition_notes: "1:1 replacement for legacy EdgeStretch. VoronoiPrism reads source_width from upstream EdgeStretch via the chain-context path; that contract is preserved when the legacy effect is migrated to this primitive in its preset graph.",
    examples: ["preset.effect.edge_stretch"],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ClampStretchUniforms {
    amount: f32,
    source_width: f32,
    mode: u32,
    _pad: f32,
}

impl Primitive for ClampStretch {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = match ctx.params.get("amount") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        // Legacy clamps source_width to [0.1, 0.9] on the CPU before
        // packing the uniform. Match that exactly — otherwise a slider
        // value outside the declared range produces different output.
        let source_width = match ctx.params.get("source_width") {
            Some(ParamValue::Float(f)) => f.clamp(0.1, 0.9),
            _ => 0.433,
        };
        // Legacy: round-and-cap to u32. Primitive accepts ParamValue::Enum
        // (already u32). Fall through to Float for tests that pass
        // raw floats matching the legacy data shape.
        let mode = match ctx.params.get("direction") {
            Some(ParamValue::Enum(v)) => (*v).min(2),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(2),
            _ => 0,
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/clamp_stretch.wgsl"),
                "cs_main",
                "node.edge_stretch",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ClampStretchUniforms {
            amount,
            source_width,
            mode,
            _pad: 0.0,
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
            "node.edge_stretch",
        );
    }
}
