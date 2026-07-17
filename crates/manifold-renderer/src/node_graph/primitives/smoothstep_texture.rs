//! `node.smoothstep` — per-pixel WGSL `smoothstep(low, high, x)`
//! on RGB. Alpha pass-through.
//!
//! The contrast-curve primitive: maps signed scalar fields (a sin sum,
//! a difference field) into `[0, 1]` with a soft S-curve at the band
//! edges. The Hermite polynomial `3t² - 2t³` clamps anything outside
//! `[low, high]` to a hard 0 or 1 and gives a smooth transition between
//! them — same behaviour as the tail of `plasma_classic`.
//!
//! Both edges are always live. There used to be a `Mode = Range |
//! Bipolar` enum where Bipolar silently ignored `low` and pinned the
//! band to `(-high, high)` — that violated the "every declared param
//! must affect output under every reachable state" rule in §7 of
//! `docs/DECOMPOSING_GENERATORS.md`. The bipolar shortcut is now a
//! graph-level pattern: wire `node.math(operation=Negate) → low` if
//! you want a symmetric-around-zero curve from a single `high` slider.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SmoothstepUniforms {
    low: f32,
    high: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: SmoothstepTexture,
    type_id: "node.smoothstep",
    purpose: "Per-pixel smoothstep contrast curve on RGB, alpha pass-through. Maps the input through `smoothstep(low, high, x)` per channel — anything below `low` clamps to 0, anything above `high` clamps to 1, and the Hermite polynomial smoothes the transition between them. Both edges are always live; for a symmetric-around-zero curve, wire `node.math(operation=Negate) → low` to mirror `high` into the low edge.",
    inputs: {
        in: Texture2D required,
        // Port-shadows-param for the two band edges so generator
        // graphs can derive contrast from outer-card sliders without
        // a Value-node middleman.
        low: ScalarF32 optional,
        high: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("low"),
            label: "Low",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-8.0, 8.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("high"),
            label: "High",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-8.0, 8.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Defaults (low=0, high=1) are identity for inputs already in [0, 1]. For Plasma-style symmetric-around-zero curves wire `node.math(operation=Negate, in=high) → low` so a single `high` slider drives both edges. `low > high` produces an inverted curve (smoothstep flips signs internally).",
    examples: [],
    picker: { label: "Smoothstep", category: Atom },
    summary: "Eases each value through a smooth S-curve between a low and high edge. Softens a hard threshold into a gentle ramp.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["smoothstep", "smoothstep texture", "ease", "s-curve", "soft threshold", "Map Range"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/smoothstep_texture_body.wgsl"),
}

impl Primitive for SmoothstepTexture {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let low = ctx.scalar_or_param("low", 0.0);
        let high = ctx.scalar_or_param("high", 1.0);

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.smoothstep standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.smoothstep",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = SmoothstepUniforms {
            low,
            high,
            _pad0: 0.0,
            _pad1: 0.0,
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
            "node.smoothstep",
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
    fn smoothstep_texture_has_no_mode_param_so_no_state_can_disable_low_or_high() {
        // Regression: the old Mode = Range | Bipolar enum gated `low`
        // off in Bipolar — a documented dead-param state. The fix was
        // to delete the mode and let users compose bipolar via a
        // negate math node. If a future commit reintroduces a mode
        // here, that's a §7 invariant violation; this test pins the
        // shape.
        let names: Vec<&str> = SmoothstepTexture::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["low", "high"]);
    }

    #[test]
    fn smoothstep_texture_registers_as_palette_atom() {
        let prim = SmoothstepTexture::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.smoothstep");
    }
}
