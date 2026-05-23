//! `node.length_vec2` — per-pixel `length(in.rg)` as a scalar field
//! in the R channel. Turns a signed flow / displacement / gradient
//! texture into a positive scalar magnitude field. Pair with
//! `node.heightmap_to_normal` (height = vec2 magnitude) for the
//! oily-fluid color → normal pipeline, with `node.smoothstep_texture`
//! for thresholding, or with a tonemap for visualisation.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: LengthVec2,
    type_id: "node.length_vec2",
    purpose: "Per-pixel `length(in.rg)` as a scalar field in the R channel (GBA = 0, 0, 1). The vec2 magnitude atom — converts signed flow / displacement / gradient textures into positive scalar fields. Standard upstream step for heightmap-style ops that need a derived height from a vec2 source.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [],
    composition_notes: "BA of `in` ignored. Output is unbounded above (length can exceed 1 for large vec2 inputs); pair with `node.gain` or `node.smoothstep_texture` to remap the range as needed. Chain: `color → length_vec2 → heightmap_to_normal` is the oily-fluid normal pipeline.",
    examples: [],
    picker: { label: "Length (vec2)", category: Atom },
}

impl Primitive for LengthVec2 {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
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
                include_str!("shaders/length_vec2.wgsl"),
                "cs_main",
                "node.length_vec2",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture: src,
                },
                GpuBinding::Sampler {
                    binding: 1,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.length_vec2",
        );
    }
}
