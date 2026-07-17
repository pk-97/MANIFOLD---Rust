//! `node.texture_sum_5` — per-pixel weighted-sum of five textures.
//!
//! `out = (a + b + c + d + e) / divisor`, all channels. `divisor=1.0`
//! (default) keeps the result a plain sum; `divisor=N` divides through —
//! the natural "average of N textures" shape (Plasma's contrast curve
//! pre-step, multi-tap composites, signed-field merges).

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TextureSum5Uniforms {
    divisor: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: TextureSum5,
    type_id: "node.texture_sum_5",
    purpose: "Per-pixel weighted-sum of five textures: out = (a+b+c+d+e) / divisor. divisor=1 keeps the result a plain sum (compose mode), divisor=5 turns it into an average. Collapses the four-deep Mix(Add) chain (+ optional scale_offset_texture for division) that a manual N-term composition would otherwise need.",
    inputs: {
        a: Texture2D required,
        b: Texture2D required,
        c: Texture2D required,
        d: Texture2D required,
        e: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("divisor"),
            label: "Divisor",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
    ],
    depth_rule: CombineNearest,
    composition_notes: "divisor=1 for a plain sum, divisor=5 for the canonical five-term average. Divide-by-zero clamps to 0 to keep the output finite. The shader does not clamp the range — a five-term sum of [-1,1] sin terms with divisor=5 lands in [-1,1] (so it feeds directly into smoothstep_bipolar without further scaling).",
    examples: [],
    summary: "Legacy fixed five-input sum, superseded by node.multi_blend (dynamic N inputs). Hidden from the palette but still loads in saved graphs.",
    category: Composite,
    role: Filter,
    aliases: ["add textures", "sum", "blend", "multi blend"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/texture_sum_5_body.wgsl"),
}

impl Primitive for TextureSum5 {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let divisor = match ctx.params.get("divisor") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };

        let Some(a) = ctx.inputs.texture_2d("a") else {
            return;
        };
        let Some(b) = ctx.inputs.texture_2d("b") else {
            return;
        };
        let Some(c) = ctx.inputs.texture_2d("c") else {
            return;
        };
        let Some(d) = ctx.inputs.texture_2d("d") else {
            return;
        };
        let Some(e) = ctx.inputs.texture_2d("e") else {
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
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.texture_sum_5 standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.texture_sum_5",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = TextureSum5Uniforms {
            divisor,
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
                GpuBinding::Texture { binding: 1, texture: a },
                GpuBinding::Texture { binding: 2, texture: b },
                GpuBinding::Texture { binding: 3, texture: c },
                GpuBinding::Texture { binding: 4, texture: d },
                GpuBinding::Texture { binding: 5, texture: e },
                GpuBinding::Sampler { binding: 6, sampler },
                GpuBinding::Texture { binding: 7, texture: out_tex },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.texture_sum_5",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn texture_sum_5_declares_five_required_texture_inputs() {
        use crate::node_graph::ports::PortType;
        assert_eq!(TextureSum5::TYPE_ID, "node.texture_sum_5");
        let ins = TextureSum5::INPUTS;
        assert_eq!(ins.len(), 5);
        for (i, name) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            assert_eq!(ins[i].name, *name);
            assert!(ins[i].required, "{name} should be required");
            assert_eq!(ins[i].ty, PortType::Texture2D);
        }
        assert_eq!(TextureSum5::OUTPUTS.len(), 1);
        assert_eq!(TextureSum5::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn texture_sum_5_has_divisor_param() {
        let names: Vec<&str> = TextureSum5::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["divisor"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = TextureSum5::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.texture_sum_5");
    }
}
