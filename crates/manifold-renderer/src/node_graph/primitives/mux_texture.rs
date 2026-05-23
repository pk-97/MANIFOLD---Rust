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
//! Cost model: short-circuits via `selected_input_branch`. When the
//! `selector` input port is UNWIRED, the executor reads the inline
//! `selector` param, resolves it to `in_N`, and prunes the producer
//! subgraphs for every other `in_K` from this frame's dispatch — the
//! 5-mode oily-fluid case goes from 5× render cost to 1×. When the
//! selector port IS wired (selector value depends on runtime-computed
//! scalars), every input subgraph runs since we can't predict the
//! selected branch without first running its producer. Live-perform
//! with an outer-card selector slider — the dominant case — uses
//! inline params so the optimisation fires.

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
    composition_notes: "Selector value rounds to nearest int, clamps to [0, 8). Selector is port-shadows-param: inline param value drives the choice when no wire is connected. If the selected in_N isn't wired the primitive falls back to in_0; if every in_N is unwired the output is cleared to opaque black so the gap is visually obvious instead of leaving sticky pool contents on the texture. Acts as a switch statement at the executor level via `EffectNode::selected_input_branch` — with the selector port unwired, only the selected branch's producer subgraph dispatches each frame. State-bearing nodes (feedback, accumulators) inside an unselected branch freeze and resume from their last value when reselected; place them outside the mux's input subgraphs if state must advance regardless of selection.",
    examples: [],
    picker: { label: "Mux (texture)", category: Atom },
}

/// Stable port-name lookup table used by both `run()` (selector-to-
/// input-texture resolution) and `selected_input_branch()` (executor's
/// live-set pruning). Keeping a single source of truth here means the
/// two paths can't drift on enum encoding (e.g. one rounding via
/// `round` while the other uses `as usize`).
const MUX_INPUT_PORT_NAMES: [&str; 8] = [
    "in_0", "in_1", "in_2", "in_3", "in_4", "in_5", "in_6", "in_7",
];

fn resolve_selector_index(selector: f32) -> usize {
    selector.round().clamp(0.0, 7.0) as usize
}

impl Primitive for MuxTexture {
    fn selected_input_branch(
        &self,
        params: &crate::node_graph::effect_node::ParamValues,
        wired_inputs: &[&str],
    ) -> Option<&'static str> {
        // Wire-driven selector: can't pre-resolve. Every branch must
        // run so whichever the wire eventually resolves to is ready.
        if wired_inputs.contains(&"selector") {
            return None;
        }
        let selector = match params.get("selector") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        Some(MUX_INPUT_PORT_NAMES[resolve_selector_index(selector)])
    }

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
        let raw_idx = resolve_selector_index(selector);

        // Find the selected input texture, with fallback to in_0 if
        // the selected slot isn't wired.
        let source = ctx
            .inputs
            .texture_2d(MUX_INPUT_PORT_NAMES[raw_idx])
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

    /// `selected_input_branch` is the trait hook the executor uses
    /// for switch-statement semantics: when the selector port is
    /// UNWIRED, the resolved-from-param branch is the only one whose
    /// producer chain runs this frame. These tests lock the param →
    /// port-name resolution so a future executor change can't drift
    /// from `run()`'s selector resolution.
    mod selected_input_branch {
        use super::*;
        use crate::node_graph::effect_node::ParamValues;

        fn prim() -> MuxTexture {
            MuxTexture::new()
        }

        fn params_with_selector(sel: f32) -> ParamValues {
            let mut p = ParamValues::default();
            p.insert("selector", ParamValue::Float(sel));
            p
        }

        #[test]
        fn unwired_selector_param_zero_picks_in_0() {
            let p = prim();
            let node: &dyn EffectNode = &p;
            assert_eq!(
                node.selected_input_branch(&params_with_selector(0.0), &[]),
                Some("in_0"),
            );
        }

        #[test]
        fn unwired_selector_param_three_picks_in_3() {
            let p = prim();
            let node: &dyn EffectNode = &p;
            assert_eq!(
                node.selected_input_branch(&params_with_selector(3.0), &[]),
                Some("in_3"),
            );
        }

        #[test]
        fn unwired_selector_param_rounds_then_clamps_within_range() {
            let p = prim();
            let node: &dyn EffectNode = &p;
            // 2.6 rounds to 3.
            assert_eq!(
                node.selected_input_branch(&params_with_selector(2.6), &[]),
                Some("in_3"),
            );
            // Over-range clamps to 7.
            assert_eq!(
                node.selected_input_branch(&params_with_selector(99.0), &[]),
                Some("in_7"),
            );
            // Negative clamps to 0.
            assert_eq!(
                node.selected_input_branch(&params_with_selector(-5.0), &[]),
                Some("in_0"),
            );
        }

        #[test]
        fn wired_selector_disables_short_circuit() {
            // `wired_inputs` containing "selector" signals to the
            // node that the selector port has a runtime-computed
            // scalar driving it — we can't predict the value, so
            // bail out and let every branch run.
            let p = prim();
            let node: &dyn EffectNode = &p;
            let wired = ["selector", "in_0", "in_1"];
            assert_eq!(
                node.selected_input_branch(&params_with_selector(2.0), &wired),
                None,
            );
        }

        #[test]
        fn run_and_selected_input_branch_agree_on_index_resolution() {
            // Cross-check: `run()` uses `resolve_selector_index` to
            // pick the texture; `selected_input_branch` uses the same
            // helper to pick the port name. Asserting they're the
            // same function avoids the bug class where one path
            // floors and the other rounds.
            for sel in [0.0_f32, 0.4, 0.6, 1.0, 3.49, 3.5, 7.0, 7.4, 7.5, 8.0, -0.1] {
                let port = MUX_INPUT_PORT_NAMES[resolve_selector_index(sel)];
                let p = prim();
                let node: &dyn EffectNode = &p;
                assert_eq!(
                    node.selected_input_branch(&params_with_selector(sel), &[]),
                    Some(port),
                    "selector={sel}: selected_input_branch must use the same \
                     resolve_selector_index helper as run()",
                );
            }
        }
    }
}
