//! `node.reinhard_tone_map` — Reinhard tone mapping on an HDR
//! Texture2D, in one of two curves selected by the `curve` enum:
//!
//! - **Extended** (default): per-channel `x * (1 + x/9) / (1 + x)`,
//!   matches the FluidSim display path bit-for-bit. Preserves more
//!   high values than Simple — visible difference on bright highlights
//!   (specular peaks).
//! - **Simple**: per-channel `x / (x + 1)`, the textbook Reinhard
//!   curve. Crushes highlights more aggressively. Matches the legacy
//!   MetallicGlass render terminal bit-for-bit.
//!
//! SDR-only. For multi-curve / HDR-aware tone mapping (ACES, AgX,
//! Khronos PBR, PQ / EDR output), use `node.tone_map` instead.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ReinhardUniforms {
    intensity: f32,
    contrast: f32,
    curve: u32, // 0 = Extended (default), 1 = Simple (x/(x+1))
    _pad0: f32,
}

crate::primitive! {
    name: ReinhardToneMap,
    type_id: "node.reinhard_tone_map",
    purpose: "Reinhard tone mapping for HDR display in one of two curves: Extended (default — `x*(1+x/9)/(1+x)`, matches FluidSim bit-for-bit, preserves highlights) or Simple (`x/(x+1)`, the textbook Reinhard curve, matches the legacy MetallicGlass render terminal bit-for-bit). intensity + contrast are port-shadowed pre-multipliers — wire a `node.canvas_area_scale → node.math` chain into `intensity` for resolution-aware brightness compensation in particle-density pipelines. SDR-only — for HDR-aware (PQ / EDR) or alternate curves (ACES / AgX / Khronos PBR Neutral), use `node.tone_map`.",
    inputs: {
        in: Texture2D required,
        intensity: ScalarF32 optional,
        contrast: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "intensity",
            label: "Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "contrast",
            label: "Contrast",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 8.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "curve",
            label: "Curve",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: &["Extended", "Simple"],
        },
    ],
    composition_notes: "intensity scales the pre-tonemap signal; contrast is a second multiplier. Both port-shadowed for runtime modulation (canvas-area brightness comp, audio-driven dynamics). Extended white-point fixed at 3.0 (FluidSim default). Simple curve is bit-exact `x/(x+1)` — picks this when matching a legacy renderer that used textbook Reinhard. Output alpha = source alpha. For HDR pipelines that need parameterised white-point or alternate curves, swap in `node.tone_map`.",
    examples: [],
    picker: { label: "Reinhard Tone Map", category: Atom },
    summary: "A simpler HDR-to-display tone map using the Reinhard curve. Lighter weight than the full Tone Map node.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["reinhard", "tonemap", "hdr"],
}

impl Primitive for ReinhardToneMap {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let intensity = ctx.scalar_or_param("intensity", 1.0);
        let contrast = ctx.scalar_or_param("contrast", 1.0);
        let curve: u32 = match ctx.params.get("curve") {
            Some(ParamValue::Enum(v)) => *v,
            _ => 0,
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
            gpu.device.create_compute_pipeline(
                include_str!("shaders/reinhard_tone_map.wgsl"),
                "cs_main",
                "node.reinhard_tone_map",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ReinhardUniforms {
            intensity,
            contrast,
            curve,
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
            "node.reinhard_tone_map",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn reinhard_declares_texture_in_and_out_plus_port_shadowed_scalars() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(ReinhardToneMap::TYPE_ID, "node.reinhard_tone_map");
        let in_port = ReinhardToneMap::INPUTS
            .iter()
            .find(|p| p.name == "in")
            .unwrap();
        assert_eq!(in_port.ty, PortType::Texture2D);
        assert!(in_port.required);

        // Port-shadows-param: intensity + contrast as optional scalar
        // inputs so a math chain (canvas_area_scale, audio-driven) can
        // drive them at runtime.
        for name in ["intensity", "contrast"] {
            let port = ReinhardToneMap::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("missing port-shadow input `{name}`"));
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
            assert!(!port.required);
        }

        assert_eq!(ReinhardToneMap::OUTPUTS.len(), 1);
        assert_eq!(ReinhardToneMap::OUTPUTS[0].name, "out");
        assert_eq!(ReinhardToneMap::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn reinhard_has_intensity_and_contrast_params() {
        let names: Vec<&str> = ReinhardToneMap::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["intensity", "contrast", "curve"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ReinhardToneMap::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.reinhard_tone_map");
    }
}
