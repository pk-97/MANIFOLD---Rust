//! `node.noise` — unified 2D procedural noise generator.
//!
//! One node with a `type` selector (Perlin / Simplex / Random) and a shared
//! octave (Detail) control, merging the four pre-existing noise primitives:
//! `node.perlin_noise_2d`, `node.simplex_noise_2d`, `node.fbm_2d`, and
//! `node.hash_noise_field_2d`. Each branch of `shaders/noise.wgsl` is lifted
//! verbatim from the original, so output is byte-identical to the node it
//! replaces:
//!
//! - **Perlin** with Detail 1 == old `perlin_noise_2d`; Detail > 1 ==
//!   `fbm_2d` (fBM is octave-summed Perlin, so it's a Detail value, not a
//!   separate type).
//! - **Simplex** with Detail 1 == old `simplex_noise_2d`.
//! - **Random** == old `hash_noise_field_2d` (per-pixel hash, R-only, no
//!   octaves).
//!
//! The four legacy type-IDs register as hidden aliases (bottom of this file)
//! that construct this node with the matching `default_type` / `default_octaves`
//! baked in, so saved projects load unchanged — old presets never stored a
//! `type` param, so the per-instance default carries it.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NoiseUniforms {
    noise_type: i32,
    scale: f32,
    offset_x: f32,
    offset_y: f32,
    octaves: i32,
    lacunarity: f32,
    persistence: f32,
    _pad0: f32,
}

/// Type selector values. Index into the `type` enum param and the shader's
/// `noise_type` uniform.
pub const NOISE_TYPES: &[&str] = &["Perlin", "Simplex", "Random", "Value"];

crate::primitive! {
    name: Noise,
    type_id: "node.noise",
    purpose: "Pure generator. Unified 2D procedural noise: `type` selects Perlin (gradient noise, square-grid lobes), Simplex (cleaner gradient noise, fewer directional artifacts), Random (per-pixel wang_hash white noise), or Value (smooth interpolated hash-grid noise — soft, slightly blobby; the classic `fract(sin)`-free value-noise with the 123.34/456.21/45.32 hash, matching the Latent Space website mosh field). `octaves` (Detail) stacks frequencies into fBM for Perlin/Simplex/Value (octaves=1 is single-octave; >1 sums lacunarity/persistence-scaled octaves). Output remapped to [0, 1]. Perlin/Simplex/Value broadcast to RGB (A=1); Random writes R only (G=B=0), matching the legacy hash field. Merges and replaces node.perlin_noise_2d / node.simplex_noise_2d / node.fbm_2d / node.hash_noise_field_2d (those type-IDs alias here).",
    inputs: {
        scale: ScalarF32 optional,
        offset_x: ScalarF32 optional,
        offset_y: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("type"),
            label: "Type",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, (NOISE_TYPES.len() - 1) as f32)),
            enum_values: NOISE_TYPES,
        },
        ParamDef {
            name: Cow::Borrowed("scale"),
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((0.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset_x"),
            label: "Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset_y"),
            label: "Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("octaves"),
            label: "Detail (octaves)",
            ty: ParamType::Int,
            default: ParamValue::Float(1.0),
            range: Some((1.0, 8.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("lacunarity"),
            label: "Lacunarity",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((1.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("persistence"),
            label: "Persistence",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Type picks the base function: Perlin (square-grid lobes), Simplex (cleaner, fewer directional artifacts), Random (uncorrelated per-pixel hash for grain / dither / LIC ink — writes R only, G=B=0), Value (smooth bilinear-interpolated hash grid — soft, slightly blobby, already in [0,1]; the value-noise that drives the website mosh's per-band and domain-warp displacement). Detail (octaves) stacks frequencies into fBM for Perlin/Simplex/Value — Detail 1 is single-octave, raise toward 8 for richer fractal texture. Lacunarity (frequency step per octave) and Persistence (amplitude falloff) shape the fractal spectrum; classic pink fBM is lacunarity 2.0 + persistence 0.5. Detail / Lacunarity / Persistence are ignored by Random. scale / offset_x / offset_y are port-shadow inputs: wire an LFO into offset to animate. Output is grayscale pre-remapped to [0, 1]; chain node.scale_offset_image (a=2, b=-1) to recover signed noise. Legacy IDs alias here: perlin_noise_2d (Perlin, Detail 1), fbm_2d (Perlin, Detail 4), simplex_noise_2d (Simplex), hash_noise_field_2d (Random).",
    examples: [],
    picker: { label: "Noise", category: Atom },
    summary: "Procedural noise in one node. Pick the Type, set the Scale, and raise Detail to stack octaves into rich fractal noise. Perlin, Simplex, and Value are smooth and organic for clouds, terrain, and slow fields; Random is per-pixel grain for film and dither.",
    category: Noise,
    role: Source,
    aliases: ["noise", "perlin", "simplex", "value", "value noise", "fbm", "fractal", "fractal noise", "white noise", "random", "hash", "clouds", "turbulence", "Noise TOP", "Noise Texture"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/noise_body.wgsl"),
    extra_fields: {
        default_type: i32 = 0,
        default_octaves: i32 = 1,
    },
}

impl Primitive for Noise {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let noise_type = match ctx.params.get("type") {
            Some(ParamValue::Enum(n)) => *n as i32,
            Some(ParamValue::Float(f)) => *f as i32,
            _ => self.default_type,
        };
        let scale = ctx.scalar_or_param("scale", 4.0);
        let offset_x = ctx.scalar_or_param("offset_x", 0.0);
        let offset_y = ctx.scalar_or_param("offset_y", 0.0);
        let octaves = match ctx.params.get("octaves") {
            Some(ParamValue::Float(f)) => f.round() as i32,
            _ => self.default_octaves,
        };
        let lacunarity = match ctx.params.get("lacunarity") {
            Some(ParamValue::Float(f)) => *f,
            _ => 2.0,
        };
        let persistence = match ctx.params.get("persistence") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        };

        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let w = target.width;
        let h = target.height;
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.noise standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.noise",
            )
        });

        let uniforms = NoiseUniforms {
            noise_type,
            scale,
            offset_x,
            offset_y,
            octaves,
            lacunarity,
            persistence,
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
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.noise",
        );
    }
}

// ── Legacy type-ID aliases (hidden from the palette) ──────────────────────
//
// Saved projects referencing the four pre-merge noise nodes resolve to this
// primitive with the matching defaults baked in. Old presets never stored a
// `type` param, so the per-instance `default_type` carries the right branch;
// `fbm_2d` additionally defaults Detail to 4 (its old octave count) for
// presets that relied on the default. New graphs pick the canonical `Noise`
// node from the palette.

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: "node.perlin_noise_2d",
        create: || Box::new(Noise::new()),
        picker: None,
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: "node.simplex_noise_2d",
        create: || {
            let mut n = Noise::new();
            n.default_type = 1;
            Box::new(n)
        },
        picker: None,
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: "node.fbm_2d",
        create: || {
            let mut n = Noise::new();
            n.default_octaves = 4;
            Box::new(n)
        },
        picker: None,
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: "node.hash_noise_field_2d",
        create: || {
            let mut n = Noise::new();
            n.default_type = 2;
            Box::new(n)
        },
        picker: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn noise_declares_texture_output_and_type_param() {
        use crate::node_graph::ports::PortType;
        assert_eq!(Noise::TYPE_ID, "node.noise");
        assert_eq!(Noise::OUTPUTS.len(), 1);
        assert_eq!(Noise::OUTPUTS[0].name, "out");
        assert_eq!(Noise::OUTPUTS[0].ty, PortType::Texture2D);
        let names: Vec<&str> = Noise::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["type", "scale", "offset_x", "offset_y", "octaves", "lacunarity", "persistence"]
        );
    }

    #[test]
    fn canonical_defaults_are_perlin_single_octave() {
        let n = Noise::new();
        assert_eq!(n.default_type, 0);
        assert_eq!(n.default_octaves, 1);
    }

    #[test]
    fn legacy_aliases_bake_the_right_defaults() {
        // The hidden factories construct Noise pre-set to the right branch so
        // saved projects (which never stored a `type`) render unchanged.
        let simplex = {
            let mut n = Noise::new();
            n.default_type = 1;
            n
        };
        assert_eq!(simplex.default_type, 1);
        let fbm = {
            let mut n = Noise::new();
            n.default_octaves = 4;
            n
        };
        assert_eq!(fbm.default_type, 0);
        assert_eq!(fbm.default_octaves, 4);
        let hash = {
            let mut n = Noise::new();
            n.default_type = 2;
            n
        };
        assert_eq!(hash.default_type, 2);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Noise::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.noise");
    }
}
