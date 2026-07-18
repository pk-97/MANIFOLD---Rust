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

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **Generated-vs-hand parity** (`docs/ADDING_PRIMITIVES.md` "The codegen
    //! path is mandatory") — the standalone kernel `run()` actually dispatches
    //! (built via `standalone_for_spec::<ToneMap>()`) must reproduce
    //! `../../effects/shaders/aces_tonemap_compute.wgsl` (the hand oracle)
    //! texel-for-texel across every (curve, mode) combination, on a
    //! synthetic HDR (>1.0) gradient so every curve's compression actually
    //! engages.
    use half::f16;

    use manifold_gpu::{
        GpuBinding, GpuComputePipeline, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc,
        GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    use super::ToneMap;
    use crate::render_target::RenderTarget;

    fn upload_rgba16f(device: &GpuDevice, w: u32, h: u32, label: &str, px: &[f16]) -> GpuTexture {
        assert_eq!(px.len(), (w * h * 4) as usize);
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label,
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px))
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    /// Non-uniform HDR gradient: every channel ranges well above 1.0 at
    /// x=w-1 so the tonemap curves' compression is actually exercised (a
    /// flat sub-1.0 fill would hide a curve-dispatch bug behind every
    /// curve's near-identity behaviour close to black).
    fn hdr_gradient(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let t = x as f32 / (w.saturating_sub(1).max(1)) as f32;
                px[i] = f16::from_f32(0.2 + t * 3.0);
                px[i + 1] = f16::from_f32(0.4 + t * 1.5);
                px[i + 2] = f16::from_f32(0.1 + (1.0 - t) * 2.0);
                px[i + 3] = f16::from_f32(1.0);
            }
        }
        upload_rgba16f(device, w, h, "tone-map-hdr-gradient", &px)
    }

    fn readback_rgba(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("tone-map-readback");
        enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared readback buffer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        (0..(w * h) as usize)
            .map(|i| {
                let o = i * 4;
                [
                    f16::from_bits(halves[o]).to_f32(),
                    f16::from_bits(halves[o + 1]).to_f32(),
                    f16::from_bits(halves[o + 2]).to_f32(),
                    f16::from_bits(halves[o + 3]).to_f32(),
                ]
            })
            .collect()
    }

    /// Dispatch a tone-map-shaped kernel (uniform(0), source(1, sampler-read),
    /// sampler(2), dst(3)) and read back the full RGBA output.
    fn dispatch_tone_map(
        device: &GpuDevice,
        pipeline: &GpuComputePipeline,
        src: &GpuTexture,
        w: u32,
        h: u32,
        uniform_bytes: &[u8],
    ) -> Vec<[f32; 4]> {
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "tone-map-out");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let mut enc = device.create_encoder("tone-map-dispatch");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform_bytes },
                GpuBinding::Texture { binding: 1, texture: src },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "tone-map-dispatch",
        );
        enc.commit_and_wait_completed();
        readback_rgba(device, &out.texture, w, h)
    }

    #[test]
    fn generated_tone_map_matches_hand_kernel_across_curves_and_modes() {
        let device = crate::test_device();
        let (w, h) = (16u32, 4u32);
        let src = hdr_gradient(&device, w, h);

        let hand_wgsl = include_str!("../../effects/shaders/aces_tonemap_compute.wgsl");
        let hand_pipeline = device.create_compute_pipeline(hand_wgsl, "cs_main", "tone-map-hand");
        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<ToneMap>()
            .expect("node.tone_map standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "tone-map-generated",
        );

        // Every (curve, mode) pair — 4 curves x 4 modes.
        for curve in 0u32..4 {
            for mode in 0u32..4 {
                let exposure = 1.3_f32;
                let paper_white = 203.0_f32;
                let max_nits = 1000.0_f32;

                // Hand-oracle layout (exposure, paper_white, max_nits, mode,
                // curve) — NOT `ToneMapUniforms`, which now follows the
                // generated PARAMS-order layout `run()` dispatches (P7 fix).
                let mut hand_bytes = Vec::new();
                hand_bytes.extend_from_slice(&exposure.to_le_bytes());
                hand_bytes.extend_from_slice(&paper_white.to_le_bytes());
                hand_bytes.extend_from_slice(&max_nits.to_le_bytes());
                hand_bytes.extend_from_slice(&mode.to_le_bytes());
                hand_bytes.extend_from_slice(&curve.to_le_bytes());
                hand_bytes.extend_from_slice(&[0u8; 12]); // pad 5 words to 8

                // The generated `Params` struct follows PARAMS declaration
                // order (exposure, curve, mode, paper_white, max_nits), which
                // is NOT the hand shader's own field order (exposure,
                // paper_white, max_nits, mode, curve) — same
                // hand-vs-generated layout split as `blinn_specular.rs`'s
                // `pack_params`, inlined here since only one atom needs it.
                let mut gen_bytes = Vec::new();
                gen_bytes.extend_from_slice(&exposure.to_le_bytes());
                gen_bytes.extend_from_slice(&curve.to_le_bytes());
                gen_bytes.extend_from_slice(&mode.to_le_bytes());
                gen_bytes.extend_from_slice(&paper_white.to_le_bytes());
                gen_bytes.extend_from_slice(&max_nits.to_le_bytes());
                gen_bytes.extend_from_slice(&[0u8; 12]); // pad 5 words to 8

                let hand_out = dispatch_tone_map(&device, &hand_pipeline, &src, w, h, &hand_bytes);
                let gen_out = dispatch_tone_map(&device, &gen_pipeline, &src, w, h, &gen_bytes);

                for (i, (h_px, g_px)) in hand_out.iter().zip(gen_out.iter()).enumerate() {
                    for c in 0..4 {
                        assert!(
                            (h_px[c] - g_px[c]).abs() < 2e-3,
                            "curve={curve} mode={mode} texel={i} ch={c}: hand={} gen={}",
                            h_px[c],
                            g_px[c]
                        );
                    }
                }
            }
        }
    }
}
