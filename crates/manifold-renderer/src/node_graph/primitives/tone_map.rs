//! `node.tone_map` — HDR → SDR/HDR display tone mapping with
//! selectable curve and output mode. Bit-exact wrap of
//! `effects/shaders/aces_tonemap_compute.wgsl` via include_str.
//!
//! Four curves:
//!   0 — Narkowicz ACES (2015)
//!   1 — Hill ACES (RRT+ODT fit in AP1, more accurate)
//!   2 — AgX (Troy Sobotka, more natural-looking)
//!   3 — Khronos PBR Neutral (preserves saturation)
//!
//! Four output modes:
//!   0 — SDR (Rec.709-ish, [0,1] range)
//!   1 — PQ (HDR10 export, [0, max_nits])
//!   2 — EDR (macOS extended dynamic range, paper-white-relative)
//!   3 — EDR passthrough (no curve, soft-clipped near display peak)
//!
//! Three pre-multipliers: `exposure` scales the input linear values
//! before tone mapping; `paper_white` is the SDR diffuse-white nit
//! target for HDR modes; `max_nits` is the display peak.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const TONE_MAP_CURVES: &[&str] = &[
    "Narkowicz ACES",
    "Hill ACES",
    "AgX",
    "Khronos PBR Neutral",
];

pub const TONE_MAP_MODES: &[&str] = &["SDR", "PQ", "EDR", "EDR Passthrough"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
// Field order MUST match the PARAMS declaration order — the runtime pipeline
// is the codegen-generated kernel, whose uniform layout is derived from
// PARAMS in order. (Same drift class as the node.shininess "weird tints" bug
// fixed in P7, 2026-07-18: this struct had mode/curve after the nit fields
// while PARAMS declare curve/mode second and third.)
struct ToneMapUniforms {
    exposure: f32,
    curve: u32,
    mode: u32,
    paper_white: f32,
    max_nits: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: ToneMap,
    type_id: "node.tone_map",
    purpose: "HDR → display tone mapping with selectable curve (Narkowicz ACES / Hill ACES / AgX / Khronos PBR Neutral) and output mode (SDR / PQ for HDR10 export / EDR for macOS / EDR passthrough). Per-channel curves, alpha pass-through. For a simple Reinhard-only path matching FluidSim's display bit-for-bit, use node.reinhard_tone_map.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("exposure"),
            label: "Exposure",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("curve"),
            label: "Curve",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: TONE_MAP_CURVES,
        },
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Output Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: TONE_MAP_MODES,
        },
        ParamDef {
            name: Cow::Borrowed("paper_white"),
            label: "Paper White (nits)",
            ty: ParamType::Float,
            default: ParamValue::Float(203.0),
            range: Some((50.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_nits"),
            label: "Max Nits",
            ty: ParamType::Float,
            default: ParamValue::Float(1000.0),
            range: Some((100.0, 10000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "For typical SDR work: leave mode=SDR, choose a curve. For HDR10 export pipelines: mode=PQ. For macOS native HDR display: mode=EDR. exposure is the linear pre-multiplier (1.0 = no change); paper_white and max_nits are only used in HDR modes. Khronos PBR Neutral preserves saturation better than ACES for very bright colors; AgX gives a more natural look at the cost of slightly muted saturation.",
    examples: [],
    picker: { label: "Tone Map", category: Atom },
    summary: "Fits HDR content, where colours can run far brighter than pure white, onto whatever display you are sending to. On a normal SDR screen or export it rolls the bright highlights down smoothly so they don't clip to flat white, and on an HDR display it can keep those highlights bright by mapping to an HDR output instead. The curve choice sets how that rolloff looks.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["tonemap", "aces", "agx", "hdr", "filmic"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/tone_map_body.wgsl"),
}

impl Primitive for ToneMap {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let exposure = match ctx.params.get("exposure") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let curve = match ctx.params.get("curve") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 0,
        };
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 0,
        };
        let paper_white = match ctx.params.get("paper_white") {
            Some(ParamValue::Float(f)) => *f,
            _ => 203.0,
        };
        let max_nits = match ctx.params.get("max_nits") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1000.0,
        };

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let width = target.width;
        let height = target.height;
        if width == 0 || height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory for per-element GPU atoms): the kernel is
            // generated from `wgsl_body` so the atom fuses. The hand shader
            // (`../../effects/shaders/aces_tonemap_compute.wgsl`) is retained
            // only as the gpu_tests parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.tone_map standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.tone_map",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ToneMapUniforms {
            exposure,
            paper_white,
            max_nits,
            mode,
            curve,
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
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.tone_map",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn tone_map_declares_texture_in_and_out() {
        use crate::node_graph::ports::PortType;
        assert_eq!(ToneMap::TYPE_ID, "node.tone_map");
        assert_eq!(ToneMap::INPUTS.len(), 1);
        assert_eq!(ToneMap::INPUTS[0].name, "in");
        assert_eq!(ToneMap::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(ToneMap::OUTPUTS.len(), 1);
        assert_eq!(ToneMap::OUTPUTS[0].name, "out");
        assert_eq!(ToneMap::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn tone_map_has_four_curves_and_four_modes() {
        let curve = ToneMap::PARAMS.iter().find(|p| p.name == "curve").unwrap();
        assert_eq!(curve.ty, ParamType::Enum);
        assert_eq!(curve.enum_values.len(), 4);

        let mode = ToneMap::PARAMS.iter().find(|p| p.name == "mode").unwrap();
        assert_eq!(mode.ty, ParamType::Enum);
        assert_eq!(mode.enum_values.len(), 4);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ToneMap::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.tone_map");
    }
}

