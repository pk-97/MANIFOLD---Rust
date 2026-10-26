//! `node.switch_array` — N-way `Array<f32>` selector.
//!
//! Sibling of `node.switch_value` and `node.switch_texture` for `Array<f32>`
//! ports. Picks one of `in_0..in_7` based on the `selector` input and
//! routes its contents into `out`. Completes the mux family planned in
//! First user is NestedCubes
//! where it gates the `target_angles` source between the pose cycler
//! and the envelope-mode accumulator.
//!
//! The mux is the documented section 7 exception to the no-dead-state rule:
//! non-selected inputs are inert by design and the user's mental model
//! accommodates this. The unwired-selected-slot case is a graph editor
//! concern (separate work).

use std::borrow::Cow;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

const PORT_NAMES: [&str; 8] = [
    "in_0", "in_1", "in_2", "in_3", "in_4", "in_5", "in_6", "in_7",
];

manifold_node_engine::primitive! {
    name: MuxArray,
    type_id: "node.switch_array",
    purpose: "N-way Array<f32> selector. Routes one of in_0..in_7 (Array f32) to the output buffer based on the selector input (rounded, clamped). With the selector set as a param, `out` is the chosen input's own buffer for the frame (no copy, no GPU work) and everything feeding only the other inputs is skipped. Output capacity = max of all wired input capacities at chain-build time. Completes the mux family alongside node.switch_value / node.switch_texture; primary use is mode-switching at the value-array level (e.g., NestedCubes Envelope vs Pose target_angles).",
    inputs: {
        selector: ScalarF32 required,
        in_0: Array(f32) optional,
        in_1: Array(f32) optional,
        in_2: Array(f32) optional,
        in_3: Array(f32) optional,
        in_4: Array(f32) optional,
        in_5: Array(f32) optional,
        in_6: Array(f32) optional,
        in_7: Array(f32) optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("selector"),
            label: "Selector",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 7.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Selector rounds to nearest int, clamps to [0, 8). Port-shadows-param: inline param drives the choice when the input wire is absent. With the selector set as a param, `out` is the chosen input's own buffer for the frame (no copy, no GPU work) and everything feeding only the other inputs is skipped. A wired selector copies the chosen input on the GPU. An unwired chosen input gives zeros.",
    examples: [],
    picker: { label: "Switch (array)", category: Atom },
    summary: "Picks one of several incoming lists and passes it through, chosen by a selector number.",
    category: Routing,
    role: Filter,
    aliases: ["switch", "mux", "mux array", "selector"],
    boundary_reason: NonGpu,
}

impl Primitive for MuxArray {
    fn selected_input_branch(
        &self,
        params: &manifold_node_engine::exec::effect_node::ParamValues,
        wired_inputs: &[&str],
    ) -> Option<&'static str> {
        // Same wired-selector rule as MuxTexture — see that primitive's
        // docstring for the rationale.
        if wired_inputs.contains(&"selector") {
            return None;
        }
        let selector = params
            .get("selector")
            .and_then(|v| v.as_scalar())
            .unwrap_or(0.0);
        let idx = selector.round().clamp(0.0, 7.0) as usize;
        Some(PORT_NAMES[idx])
    }

    /// With a param selector, a wired choice passes through by alias: the executor
    /// points `out` at the chosen input's buffer and live extent for the frame
    /// and `run` does not execute. An unwired choice runs, to publish zeros.
    fn skip_passthrough(
        &self,
        params: &manifold_node_engine::exec::effect_node::ParamValues,
        wired_inputs: &[&str],
    ) -> Option<(&'static str, &'static str)> {
        let port = self.selected_input_branch(params, wired_inputs)?;
        wired_inputs.contains(&port).then_some((port, "out"))
    }

    fn variadic_skip_passthrough_out(&self) -> Option<&'static str> {
        Some("out")
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        // Output sized to the max of any wired input — covers the
        // case where the selector picks an input that's bigger than
        // others. Unwired inputs don't appear in input_capacities.
        let max = input_capacities
            .iter()
            .filter(|(p, _)| PORT_NAMES.contains(p))
            .map(|(_, n)| *n)
            .max()
            .unwrap_or(1);
        Some(max.max(1))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let selector_scalar = ctx
            .inputs
            .scalar("selector")
            .and_then(|v| v.as_scalar())
            .unwrap_or_else(|| {
                ctx.params
                    .get("selector")
                    .and_then(|v| v.as_scalar())
                    .unwrap_or(0.0)
            });
        let idx = selector_scalar.round().clamp(0.0, 7.0) as usize;

        let Some(dst) = ctx.outputs.array("out") else {
            log::warn!(
                "node.switch_array: no GpuBuffer bound to output port `out` — \
                 the chain build did not pre-allocate the Array<f32> output.",
            );
            return;
        };
        let dst_cap_bytes = dst.size as usize;
        if dst_cap_bytes == 0 {
            return;
        }

        let src_port = PORT_NAMES[idx];
        let src = ctx.inputs.array(src_port);
        let gpu = ctx.gpu_encoder();
        let Some(src) = src else {
            // Selected input unwired: output zero-filled. The selector
            // chose a slot the graph didn't provide; emitting zeros is
            // the least-surprising behaviour (matches mux_scalar's
            // unwired-slot returning 0.0).
            gpu.native_enc.clear_buffer(dst);
            return;
        };
        let copy_len = src.size.min(dst.size);

        // A source smaller than `dst` leaves a tail, so clear `dst` first
        // to keep stale data from a previously-selected larger input out of
        // downstream reads that look at the full capacity.
        if copy_len < dst.size {
            gpu.native_enc.clear_buffer(dst);
        }
        gpu.native_enc.copy_buffer_range(src, 0, dst, 0, copy_len);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_param_selector_passes_the_wired_choice_through() {
        use manifold_node_engine::exec::effect_node::ParamValues;
        let prim = MuxArray::new();
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("selector"), ParamValue::Float(1.0));
        assert_eq!(
            Primitive::skip_passthrough(&prim, &params, &["in_0", "in_1"]),
            Some(("in_1", "out"))
        );
        // Unwired choice runs, so it can publish zeros.
        assert_eq!(Primitive::skip_passthrough(&prim, &params, &["in_0"]), None);
        // A wired selector keeps every branch live.
        assert_eq!(
            Primitive::skip_passthrough(&prim, &params, &["selector", "in_0", "in_1"]),
            None
        );
    }

    #[test]
    fn array_output_capacity_is_max_of_wired_inputs() {
        use manifold_node_engine::exec::effect_node::ParamValues;
        let prim = MuxArray::new();
        let params = ParamValues::default();
        // Two inputs wired with different capacities; out should
        // take the larger.
        let inputs = [("in_0", 5u32), ("in_1", 8u32)];
        let cap = Primitive::array_output_capacity(&prim, "out", &params, &inputs);
        assert_eq!(cap, Some(8));
    }

    #[test]
    fn array_output_capacity_defaults_to_one_when_no_inputs_wired() {
        use manifold_node_engine::exec::effect_node::ParamValues;
        let prim = MuxArray::new();
        let params = ParamValues::default();
        // No inputs wired — capacity must still be ≥ 1 so the
        // pre-allocator doesn't refuse the slot.
        let cap = Primitive::array_output_capacity(&prim, "out", &params, &[]);
        assert_eq!(cap, Some(1));
    }
}
