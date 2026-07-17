//! `node.normalize` — per-pixel safe-normalize of the RG
//! channels of an input texture as a 2D vector.
//!
//! Reads `in.rg` per pixel, writes `(v / length(v), 0, 1)` when
//! `length > 1e-6`, else `(0, 0, 0, 1)`. Direction-only output —
//! caller restores magnitude with downstream `node.exposure` if needed.
//! The classic safe-normalize is the building block for curl-forcing
//! extraction in reaction-diffusion / fluid sims and for any
//! flow-field op that wants directional uniformity regardless of
//! source magnitude.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: NormalizeVec2,
    type_id: "node.normalize",
    purpose: "Per-pixel safe-normalize of the input's RG channels treated as a vec2. Writes (v/length(v), 0, 1) when length ≥ 1e-6, else (0, 0, 0, 1). Direction-only — restores magnitude downstream with `node.exposure`. The building block for curl-force extraction (normalize gradients before summing in fluid-sim velocity steps) and any flow-field that wants directional uniformity regardless of source magnitude.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [],
    depth_rule: Inherit,
    composition_notes: "BA of the input are ignored; output BA is forced to (0, 1). Chain order for the oily-fluid curl-force pattern: `node.edge_slope → node.normalize → (sum two of these) → node.exposure → node.rotate_vector`.",
    examples: [],
    picker: { label: "Normalize", category: Atom },
    summary: "Scales the red and green channels read as a 2D vector down to length 1, keeping the direction and dropping the magnitude.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["normalize", "normalize vec2", "unit vector", "direction"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/normalize_vec2_body.wgsl"),
}

impl Primitive for NormalizeVec2 {
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
            // Paramless Pointwise. Generated kernel binds tex(0)/samp(1)/dst(2).
            // normalize_vec2.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.normalize standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.normalize",
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
            "node.normalize",
        );
    }
}
