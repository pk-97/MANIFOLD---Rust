//! `node.smoothstep_texture` — per-pixel WGSL `smoothstep(low, high, x)`
//! on RGB. Alpha pass-through.
//!
//! The contrast-curve primitive: maps signed scalar fields (a sin sum,
//! a difference field) into `[0, 1]` with a soft S-curve at the band
//! edges. The Hermite polynomial `3t² - 2t³` clamps anything outside
//! `[low, high]` to a hard 0 or 1 and gives a smooth transition between
//! them — same behaviour as the tail of `plasma_classic`.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SmoothstepUniforms {
    low: f32,
    high: f32,
    mode: u32,
    _pad0: f32,
}

pub const SMOOTHSTEP_MODES: &[&str] = &["Range", "Bipolar"];

crate::primitive! {
    name: SmoothstepTexture,
    type_id: "node.smoothstep_texture",
    purpose: "Per-pixel smoothstep contrast curve on RGB, alpha pass-through. Mode=Range uses (low, high); Mode=Bipolar uses (-high, high) so a single `high` slider gives a symmetric curve around 0 (the canonical signed-field → [0,1] remap used by Plasma's contrast curve and similar sum-of-sines patterns).",
    inputs: {
        in: Texture2D required,
        // Port-shadows-param for the two band edges so generator
        // graphs can derive contrast from outer-card sliders without
        // a Value-node middleman. `low` is ignored in Bipolar mode.
        low: ScalarF32 optional,
        high: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "low",
            label: "Low",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-8.0, 8.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "high",
            label: "High",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-8.0, 8.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "mode",
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0), // Range
            range: Some((0.0, (SMOOTHSTEP_MODES.len() - 1) as f32)),
            enum_values: SMOOTHSTEP_MODES,
        },
    ],
    composition_notes: "Defaults (low=0, high=1, mode=Range) are identity for inputs already in [0, 1]. For Plasma-style symmetric-around-zero curves use mode=Bipolar and drive `high` from the edge value (low is ignored). `mode=Range` with low=-high gives the same result without the bipolar shortcut.",
    examples: [],
    picker: { label: "Smoothstep", category: Atom },
}

impl Primitive for SmoothstepTexture {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let low = ctx.scalar_or_param("low", 0.0);
        let high = ctx.scalar_or_param("high", 1.0);
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => (*v).min((SMOOTHSTEP_MODES.len() - 1) as u32),
            Some(ParamValue::Float(f)) => (f.round() as u32).min((SMOOTHSTEP_MODES.len() - 1) as u32),
            _ => 0,
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/smoothstep_texture.wgsl"),
                "cs_main",
                "node.smoothstep_texture",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = SmoothstepUniforms {
            low,
            high,
            mode,
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
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.smoothstep_texture",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn smoothstep_texture_declares_required_texture_plus_two_optional_scalar_inputs() {
        use crate::node_graph::ports::{PortType, ScalarType};
        let ins = SmoothstepTexture::INPUTS;
        assert_eq!(ins.len(), 3);
        assert_eq!(ins[0].name, "in");
        assert!(ins[0].required);
        assert_eq!(ins[0].ty, PortType::Texture2D);
        assert_eq!(ins[1].name, "low");
        assert!(!ins[1].required);
        assert_eq!(ins[1].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(ins[2].name, "high");
        assert!(!ins[2].required);
        assert_eq!(ins[2].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(SmoothstepTexture::OUTPUTS.len(), 1);
    }

    #[test]
    fn smoothstep_texture_registers_as_palette_atom() {
        let prim = SmoothstepTexture::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.smoothstep_texture");
    }
}
