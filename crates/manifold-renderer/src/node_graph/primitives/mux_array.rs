//! `node.switch_array` — N-way `Array<f32>` selector.
//!
//! Sibling of `node.switch_value` and `node.switch_texture` for `Array<f32>`
//! ports. Picks one of `in_0..in_7` based on the `selector` input and
//! routes its contents into `out`. Completes the mux family planned in
//! `docs/GENERATOR_DECOMPOSITION_PLAN.md` D2; first user is NestedCubes
//! where it gates the `target_angles` source between the pose cycler
//! and the envelope-mode accumulator.
//!
//! The mux is the documented §7 exception to the no-dead-state rule:
//! non-selected inputs are inert by design and the user's mental model
//! accommodates this. The unwired-selected-slot case is a graph editor
//! concern (separate work).

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const PORT_NAMES: [&str; 8] = [
    "in_0", "in_1", "in_2", "in_3", "in_4", "in_5", "in_6", "in_7",
];

crate::primitive! {
    name: MuxArray,
    type_id: "node.switch_array",
    purpose: "N-way Array<f32> selector. Routes one of in_0..in_7 (Array f32) to the output buffer based on the selector input (rounded, clamped). Output capacity = max of all wired input capacities at chain-build time. Completes the mux family alongside node.switch_value / node.switch_texture; primary use is mode-switching at the value-array level (e.g., NestedCubes Envelope vs Pose target_angles).",
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
    composition_notes: "Selector rounds to nearest int, clamps to [0, 8). Port-shadows-param: inline param drives the choice when the input wire is absent. The unwired-selected-slot case yields a zero-filled output. Same-frame caveat: if a selected input is GPU-written by an upstream compute primitive, the mux's CPU memcpy may miss the write without an explicit fence — for the first user (NestedCubes) both upstream sources are CPU-write so this isn't an issue, but a GPU-side copy variant is the right shape if a future consumer needs it.",
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
        params: &crate::node_graph::effect_node::ParamValues,
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

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
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
        let Some(src) = ctx.inputs.array(src_port) else {
            // Selected input unwired — output zero-filled. The selector
            // chose a slot the graph didn't provide; emitting zeros is
            // the least-surprising behaviour (matches mux_scalar's
            // unwired-slot returning 0.0).
            let zeros = vec![0u8; dst_cap_bytes];
            unsafe { dst.write(0, &zeros) };
            return;
        };
        let src_size = src.size as usize;
        let copy_len = src_size.min(dst_cap_bytes);

        // CPU memcpy from src to dst. Both buffers are shared-memory
        // by the Array<T> pre-allocation policy. Same-frame correctness
        // depends on upstream being CPU-write (true for the cycler and
        // the accumulator); see composition_notes.
        let Some(src_ptr) = src.mapped_ptr() else {
            log::warn!("node.switch_array: source buffer has no mapped_ptr");
            return;
        };
        // Safety: src_ptr is valid for src_size bytes (allocation policy),
        // copy_len is clamped to both src and dst sizes, and the
        // executor runs primitives sequentially on the content thread
        // so no concurrent reader races this write.
        let src_slice = unsafe { std::slice::from_raw_parts(src_ptr, copy_len) };
        unsafe { dst.write(0, src_slice) };

        // Zero-fill any tail beyond the source's size — keeps stale
        // data from a previously-selected larger input from leaking
        // into downstream reads that look at the full capacity.
        if copy_len < dst_cap_bytes {
            let tail = vec![0u8; dst_cap_bytes - copy_len];
            unsafe { dst.write(copy_len as u64, &tail) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_one_required_selector_and_eight_optional_array_inputs() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};

        let inputs = MuxArray::INPUTS;
        assert_eq!(inputs.len(), 9);
        assert_eq!(inputs[0].name, "selector");
        assert!(inputs[0].required);
        assert_eq!(inputs[0].ty, PortType::Scalar(ScalarType::F32));

        let array_layout = PortType::Array(ArrayType::of_known::<f32>());
        for port in inputs.iter().skip(1) {
            assert!(!port.required);
            assert_eq!(port.ty, array_layout);
        }
        assert_eq!(MuxArray::OUTPUTS.len(), 1);
        assert_eq!(MuxArray::OUTPUTS[0].name, "out");
        assert_eq!(MuxArray::OUTPUTS[0].ty, array_layout);
    }

    #[test]
    fn array_output_capacity_is_max_of_wired_inputs() {
        use crate::node_graph::effect_node::ParamValues;
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
        use crate::node_graph::effect_node::ParamValues;
        let prim = MuxArray::new();
        let params = ParamValues::default();
        // No inputs wired — capacity must still be ≥ 1 so the
        // pre-allocator doesn't refuse the slot.
        let cap = Primitive::array_output_capacity(&prim, "out", &params, &[]);
        assert_eq!(cap, Some(1));
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        use crate::node_graph::palette::{PaletteCategory, palette_atoms};
        let atoms = palette_atoms();
        let entry = atoms
            .iter()
            .find(|e| e.type_id == MuxArray::TYPE_ID)
            .expect("mux_array should be registered as a palette atom");
        assert_eq!(entry.label, "Switch (array)");
        assert!(matches!(entry.category, PaletteCategory::Atom));
    }
}
