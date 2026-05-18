//! `node.voronoi_prism` — pixel-exact replacement for legacy
//! Originally `VoronoiPrismFX`.
//! Twelfth §6.1 migration; fused composite.
//!
//! Per-cell UV remapping with beat-synchronized pop-in and per-cell
//! visibility hash (~40% of cells go dark per beat). `source_width`
//! is a regular parameter — previously this was a hidden cross-read
//! from an upstream EdgeStretch's width slider, but the splice
//! migration replaced that with an explicit slider on the
//! VoronoiPrism card.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: VoronoiPrism,
    type_id: "node.voronoi_prism",
    purpose: "Per-cell UV remapping with beat-synchronized pop-in. Each Voronoi cell samples a hash-offset source UV and fades on at the start of each beat; ~40% of cells go dark each beat for a flicker pattern.",
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
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cell_count",
            label: "Cell Count",
            ty: ParamType::Int,
            default: ParamValue::Int(16),
            range: Some((4.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "beat",
            label: "Beat",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1e9)),
            enum_values: &[],
        },
        ParamDef {
            name: "source_width",
            label: "Source Width",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.1, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Fused composite — atomic VoronoiCells + per-cell-hash + Mix would round through fp16 between passes and break parity. The legacy effect pulls source_width from an upstream EdgeStretch via the chain context; the primitive surfaces it explicitly so the preset graph wires it directly.",
    examples: ["preset.effect.voronoi_prism"],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VoronoiPrismUniforms {
    amount: f32,
    cell_count: f32,
    beat: f32,
    aspect_ratio: f32,
    source_width: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

impl Primitive for VoronoiPrism {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = read_f32(ctx, "amount", 0.0);
        let cell_count = read_f32(ctx, "cell_count", 16.0);
        let beat = read_f32(ctx, "beat", 0.0);
        let source_width = read_f32(ctx, "source_width", 0.5625);

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);
        let aspect_ratio = width as f32 / height as f32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/voronoi_prism.wgsl"),
                "cs_main",
                "node.voronoi_prism",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = VoronoiPrismUniforms {
            amount,
            cell_count,
            beat,
            aspect_ratio,
            source_width,
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
            "node.voronoi_prism",
        );
    }
}

fn read_f32(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
    match ctx.params.get(name) {
        Some(ParamValue::Float(f)) => *f,
        _ => default,
    }
}
