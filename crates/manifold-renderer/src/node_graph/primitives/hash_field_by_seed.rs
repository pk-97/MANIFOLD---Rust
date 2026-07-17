//! `node.hash_field_by_seed` — hash an input value-field (its RG
//! channels) with an added scalar `seed`, so the same field re-rolls as
//! the seed changes:
//!   seeded = field.rg + seed · (seed_x, seed_y)
//!   Hash2 (mode 0): out.rg = hash2(seeded)   ∈ [0,1]²
//!   Hash1 (mode 1): out.rgb = hash1(seeded)  ∈ [0,1]
//!
//! The "re-hash a value field by a seed" atom the §2.5 audit found
//! missing. Feed `node.voronoi_2d`'s `cell_id` output (RG) and a
//! `beat_floor` seed to get per-cell randoms that jump each beat — the
//! per-beat reshuffle at the heart of Voronoi Prism. General: any value
//! field + any seed.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const HASH_FIELD_MODES: &[&str] = &["Hash2", "Hash1"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HashFieldUniforms {
    seed: f32,
    seed_x: f32,
    seed_y: f32,
    mode: u32,
}

crate::primitive! {
    name: HashFieldBySeed,
    type_id: "node.hash_field_by_seed",
    purpose: "Hash an input value-field's RG channels with an added scalar seed: seeded = field.rg + seed·(seed_x, seed_y); Hash2 (mode 0) → out.rg = hash2(seeded) in [0,1]^2, Hash1 (mode 1) → out.rgb = hash1(seeded) in [0,1]. The 're-hash a value field by a seed' atom — feed node.voronoi_2d's cell_id output (RG) + a beat_floor seed to get per-cell randoms that re-roll each beat (Voronoi Prism's per-beat content shuffle / visibility). General: any value field, any seed.",
    inputs: {
        field: Texture2D required,
        seed: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("seed"),
            label: "Seed",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1e9)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("seed_x"),
            label: "Seed X",
            ty: ParamType::Float,
            default: ParamValue::Float(1.73),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("seed_y"),
            label: "Seed Y",
            ty: ParamType::Float,
            default: ParamValue::Float(2.91),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 1.0)),
            enum_values: HASH_FIELD_MODES,
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Reads `field` via textureLoad (no interpolation) so a per-cell-constant field (voronoi_2d's cell_id) stays exact across cell boundaries — field and output must be the same resolution (true in a single-canvas graph). hash2 / hash1 constants are verbatim from the legacy Voronoi Prism. `seed` port-shadows the param: wire generator_input.beat → node.math(Floor) for the per-beat reshuffle. seed_x/seed_y set the per-axis seed weights (prism offset uses 1.73/2.91, visibility uses 0.17/0.31).",
    examples: ["preset.effect.voronoi_prism"],
    picker: { label: "Hash Field by Seed", category: Atom },
    summary: "Scrambles a coordinate field by a seed so the same input gives a different but stable random offset per seed. Used to re-randomise a pattern on a trigger.",
    category: FieldsAndCoordinates,
    role: Map,
    aliases: ["hash", "randomise", "seed", "scramble"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/hash_field_by_seed_body.wgsl"),
    input_access: [CoincidentTexel],
}

impl Primitive for HashFieldBySeed {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let seed = match ctx.inputs.scalar("seed") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("seed") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
        };
        let read = |name: &str, default: f32| -> f32 {
            match ctx.params.get(name) {
                Some(ParamValue::Float(f)) => *f,
                _ => default,
            }
        };
        let seed_x = read("seed_x", 1.73);
        let seed_y = read("seed_y", 2.91);
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => (*v).min(1),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(1),
            _ => 0,
        };

        let Some(field) = ctx.inputs.texture_2d("field") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // `field` is a CoincidentTexel input (own-texel integer textureLoad, no
            // sampler). Generated kernel binds uniform(0)/tex(1)/dst(2).
            // hash_field_by_seed.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.hash_field_by_seed standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.hash_field_by_seed",
            )
        });

        let uniforms = HashFieldUniforms {
            seed,
            seed_x,
            seed_y,
            mode,
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
                    texture: field,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.hash_field_by_seed",
        );
    }
}
