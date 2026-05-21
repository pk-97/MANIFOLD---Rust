//! `node.plasma_pattern_2d` — the eight Plasma pattern variants packed
//! into one primitive. Bit-exact port of the legacy `PlasmaGenerator`
//! shader, with the `pattern` enum picking which variant runs and the
//! rest of the params (`complexity`, `contrast`, `speed`, `scale`)
//! shared across all variants.
//!
//! This is the "TD Noise TOP" analog for the Plasma family — a single
//! curated node that covers the whole family rather than decomposing
//! every variant into sin-term plumbing. Future curated families
//! (Marble, Cellular, Voronoi-driven, etc.) get their own
//! `*_pattern_2d` primitive on the same pattern.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Number of pattern variants the primitive covers. Mirrors the legacy
/// generator's `PATTERN_COUNT` so clip-trigger cycling lands on the same
/// indices.
pub const PLASMA_PATTERN_COUNT: u32 = 8;

pub const PLASMA_PATTERNS: &[&str] = &[
    "Classic", "Rings", "Diamond", "Warp", "Cells", "Noise", "Fractal", "Lattice",
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PlasmaPatternUniforms {
    time: f32,
    aspect_ratio: f32,
    anim_speed: f32,
    uv_scale: f32,
    pattern_type: f32,
    complexity: f32,
    contrast: f32,
    trigger_count: f32,
}

crate::primitive! {
    name: PlasmaPattern2D,
    type_id: "node.plasma_pattern_2d",
    purpose: "Curated Plasma pattern primitive — eight bit-exact algorithm variants (Classic / Rings / Diamond / Warp / Cells / Noise / Fractal / Lattice) selected by the `pattern` enum, with shared complexity / contrast / speed / scale params. The whole Plasma family in one node, the way TD's Noise TOP packs many noise types behind one operator.",
    inputs: {
        // Standard generator-input scalars, port-shadowable so a
        // generator graph can drive them from system.generator_input.
        time: ScalarF32 optional,
        aspect: ScalarF32 optional,
        trigger_count: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "pattern",
            label: "Pattern",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0), // Classic
            range: Some((0.0, (PLASMA_PATTERN_COUNT - 1) as f32)),
            enum_values: PLASMA_PATTERNS,
        },
        ParamDef {
            name: "complexity",
            label: "Complexity",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "contrast",
            label: "Contrast",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "speed",
            label: "Speed",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "scale",
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.1, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "clip_trigger",
            label: "Clip Trigger",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: "time",
            label: "Time (base)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "aspect",
            label: "Aspect Ratio",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.1, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "trigger_count",
            label: "Trigger Count",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1000.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Wire `time` from system.generator_input.time and `aspect` from system.generator_input.aspect for the standard generator setup. When `clip_trigger = true` the active pattern cycles by `trigger_count % 8` instead of the static `pattern` param — wire trigger_count from system.generator_input.trigger_count to drive per-retrigger switching from a NoteOn source. `speed` scales time; `scale` is inverted internally so larger values zoom out. Contrast = 0 gives the widest band, contrast = 1 the sharpest threshold.",
    examples: [],
    picker: { label: "Plasma Pattern 2D", category: Atom },
}

fn read_scalar(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
    match ctx.inputs.scalar(name) {
        Some(ParamValue::Float(f)) => f,
        _ => match ctx.params.get(name) {
            Some(ParamValue::Float(f)) => *f,
            _ => default,
        },
    }
}

impl Primitive for PlasmaPattern2D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let time = read_scalar(ctx, "time", 0.0);
        let aspect = read_scalar(ctx, "aspect", 1.0);
        let trigger_count = read_scalar(ctx, "trigger_count", 0.0);

        let pattern_param = match ctx.params.get("pattern") {
            Some(ParamValue::Enum(v)) => *v,
            Some(ParamValue::Float(f)) => (f.round() as u32).min(PLASMA_PATTERN_COUNT - 1),
            _ => 0,
        };
        let complexity = match ctx.params.get("complexity") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        };
        let contrast = match ctx.params.get("contrast") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        };
        let speed = match ctx.params.get("speed") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let scale = match ctx.params.get("scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        // `clip_trigger` is declared as a Bool param, but the outer-card
        // binding feeds it via `convert: Float` so the value can arrive
        // as Bool(true) or Float(>0.5). Match the legacy generator's
        // `params[CLIP_TRIGGER] > 0.5` semantics so the toggle actually engages
        // regardless of which type the binding writes.
        let clip_trigger = match ctx.params.get("clip_trigger") {
            Some(ParamValue::Bool(b)) => *b,
            Some(ParamValue::Float(f)) => *f > 0.5,
            Some(ParamValue::Int(i)) => *i != 0,
            _ => false,
        };

        // Clip-trigger mode overrides the static pattern with trigger_count
        // modulo the pattern count — matches the legacy generator's
        // CPU-side resolution exactly.
        let pattern_type = if clip_trigger {
            (trigger_count.floor() as i64).rem_euclid(PLASMA_PATTERN_COUNT as i64) as f32
        } else {
            pattern_param as f32
        };

        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };

        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/plasma_pattern_2d.wgsl"),
                "cs_main",
                "node.plasma_pattern_2d",
            )
        });

        let uniforms = PlasmaPatternUniforms {
            time,
            aspect_ratio: aspect,
            anim_speed: speed,
            uv_scale,
            pattern_type,
            complexity,
            contrast,
            trigger_count,
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
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.plasma_pattern_2d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn plasma_pattern_2d_declares_three_optional_scalar_inputs_and_one_texture_output() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(PlasmaPattern2D::TYPE_ID, "node.plasma_pattern_2d");
        let ins = PlasmaPattern2D::INPUTS;
        assert_eq!(ins.len(), 3);
        for (i, name) in ["time", "aspect", "trigger_count"].iter().enumerate() {
            assert_eq!(ins[i].name, *name);
            assert!(!ins[i].required);
            assert_eq!(ins[i].ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(PlasmaPattern2D::OUTPUTS.len(), 1);
        assert_eq!(PlasmaPattern2D::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn plasma_pattern_2d_covers_eight_pattern_variants() {
        assert_eq!(PLASMA_PATTERNS.len(), PLASMA_PATTERN_COUNT as usize);
        assert_eq!(PLASMA_PATTERNS[0], "Classic");
        assert_eq!(PLASMA_PATTERNS[7], "Lattice");
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = PlasmaPattern2D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.plasma_pattern_2d");
    }
}
