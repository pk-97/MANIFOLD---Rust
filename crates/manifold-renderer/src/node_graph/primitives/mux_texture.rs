//! `node.mux_texture` — N-way texture selector.
//!
//! Variadic-input primitive with up to 8 optional `Texture2D` inputs
//! (`in_0` through `in_7`) and one scalar `selector` (Scalar F32). The
//! selector value rounds to the nearest integer, clamps to `[0, 8)`,
//! and forwards the matching input's contents into the output. Inputs
//! that aren't wired round-robin down to `in_0`; if `in_0` isn't wired
//! either the output stays untouched for the frame.
//!
//! Designed for clip-trigger-style preset cycling (Plasma's 8 patterns,
//! ConcentricTunnel's 6 shapes, etc.). The host wires
//! `system.generator_input.trigger_count → selector` and each
//! sub-graph variant into `in_0..in_N`.
//!
//! Cost model: all upstream branches still dispatch every frame in v1.
//! A future planner pass can skip unselected sub-graphs when the
//! selector is statically known.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const MUX_TEXTURE_WGSL: &str = r"
@group(0) @binding(0) var source_tex: texture_2d<f32>;
@group(0) @binding(1) var tex_sampler: sampler;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let color = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    textureStore(output_tex, vec2<i32>(id.xy), color);
}
";

crate::primitive! {
    name: MuxTexture,
    type_id: "node.mux_texture",
    purpose: "N-way Texture2D selector. Routes one of in_0..in_7 to the output based on the selector scalar input (rounded, clamped). Use for clip-trigger-style preset cycling — wire generator_input.trigger_count to selector and each variant sub-graph to in_0..in_N.",
    inputs: {
        selector: ScalarF32 required,
        in_0: Texture2D optional,
        in_1: Texture2D optional,
        in_2: Texture2D optional,
        in_3: Texture2D optional,
        in_4: Texture2D optional,
        in_5: Texture2D optional,
        in_6: Texture2D optional,
        in_7: Texture2D optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "selector",
            label: "Selector",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 7.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Selector value rounds to nearest int, clamps to [0, 8). Selector is port-shadows-param: inline param value drives the choice when no wire is connected. If the selected in_N isn't wired the primitive falls back to in_0; if every in_N is unwired the output is cleared to opaque black so the gap is visually obvious instead of leaving sticky pool contents on the texture. All upstream sub-graphs still execute every frame — a future planner pass can prune unselected branches when the selector is statically known. Mux-shaped 'input selection' is the documented §7 exception to the no-dead-state rule — the user's mental model of a mux accommodates non-selected inputs being inert; the unwired-selected-slot case is a graph-editor authoring concern (separate work).",
    examples: [],
    picker: { label: "Mux (texture)", category: Atom },
}

impl Primitive for MuxTexture {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Read the selector. Port-shadows-param: wired value overrides
        // the inline param, otherwise the param drives the choice.
        let selector = match ctx.inputs.scalar("selector") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("selector") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
        };
        let raw_idx = selector.round().clamp(0.0, 7.0) as usize;

        // Find the selected input texture, with fallback to in_0 if
        // the selected slot isn't wired.
        let port_names = [
            "in_0", "in_1", "in_2", "in_3", "in_4", "in_5", "in_6", "in_7",
        ];
        let source = ctx
            .inputs
            .texture_2d(port_names[raw_idx])
            .or_else(|| ctx.inputs.texture_2d("in_0"));

        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out.width, out.height);

        // Every in_N unwired: clear the output to opaque black so the
        // gap is visually obvious. Pre-fix this path silently left the
        // pool's last frame on the texture — looked like a stuck
        // render to the user.
        let Some(source) = source else {
            let gpu = ctx.gpu_encoder();
            gpu.clear_texture(out, 0.0, 0.0, 0.0, 1.0);
            return;
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                MUX_TEXTURE_WGSL,
                "cs_main",
                "node.mux_texture",
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
                    texture: source,
                },
                GpuBinding::Sampler {
                    binding: 1,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: out,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.mux_texture",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn mux_texture_declares_one_required_selector_and_eight_optional_inputs() {
        use crate::node_graph::ports::{PortType, ScalarType};
        let inputs = MuxTexture::INPUTS;
        assert_eq!(inputs.len(), 9);
        assert_eq!(inputs[0].name, "selector");
        assert!(inputs[0].required);
        assert_eq!(inputs[0].ty, PortType::Scalar(ScalarType::F32));
        for (i, port) in inputs.iter().enumerate().skip(1) {
            assert_eq!(port.name, format!("in_{}", i - 1));
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Texture2D);
        }
        assert_eq!(MuxTexture::OUTPUTS.len(), 1);
        assert_eq!(MuxTexture::OUTPUTS[0].name, "out");
    }

    #[test]
    fn mux_texture_registers_with_palette() {
        let prim = MuxTexture::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.mux_texture");
    }
}
