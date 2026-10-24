//! `node.switch_indices` — two-way `Array<u32>` selector: the
//! index-buffer twin of `node.switch_mesh`, which owns the shared passthrough
//! and run path ([`ArraySwitch`]).

use std::borrow::Cow;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

use super::switch_mesh::{
    ArraySwitch, selected_switch_input, switch_output_capacity, switch_passthrough,
};

manifold_node_engine::primitive! {
    name: SwitchIndices,
    type_id: "node.switch_indices",
    purpose: "Two-way Array<u32> selector for triangle index buffers. Passes in_0 or in_1 through to out with the chosen input's live extent, chosen by the selector (rounded, clamped). With the selector set as a param, out is the chosen input's own buffer for the frame (no copy, no GPU work) and everything feeding only the other input is skipped; a wired selector blits the chosen live indices on the GPU. Output capacity = the larger wired input.",
    inputs: {
        selector: ScalarF32 required,
        in_0: Array(u32) optional,
        in_1: Array(u32) optional,
    },
    outputs: {
        out: Array(u32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("selector"),
            label: "Selector",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Pair with node.switch_mesh on the vertices wire of the same node.scene_object, both with the same selector set as a param (port-shadowed: a wired selector keeps both branches running). The output carries the chosen input's live extent, which controls the object's indexed draw, so it draws exactly the triangles the direct wire would. A chosen input that is unwired draws nothing (live extent zero).",
    examples: [],
    picker: { label: "Switch (indices)", category: Atom },
    summary: "Picks one of two incoming triangle index lists and passes it through, chosen by a selector number. The list it does not pick is not computed.",
    category: Routing,
    role: Filter,
    aliases: ["switch", "mux", "index switch", "selector"],
    boundary_reason: NonGpu,
    extra_fields: {
        switch: ArraySwitch = ArraySwitch::default(),
    },
}

manifold_node_engine::param_tooltips!("node.switch_indices", {
    "selector" => "Picks which index list to pass through: 0 or 1.",
});

impl Primitive for SwitchIndices {
    fn selected_input_branch(
        &self,
        params: &ParamValues,
        wired_inputs: &[&str],
    ) -> Option<&'static str> {
        selected_switch_input(params, wired_inputs)
    }

    fn skip_passthrough(
        &self,
        params: &ParamValues,
        wired_inputs: &[&str],
    ) -> Option<(&'static str, &'static str)> {
        switch_passthrough(params, wired_inputs)
    }

    fn variadic_skip_passthrough_out(&self) -> Option<&'static str> {
        Some("out")
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        switch_output_capacity(port_name, input_capacities)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.switch.run(
            ctx,
            std::mem::size_of::<u32>() as u64,
            "node.switch_indices",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(test)]
mod tests {
    use manifold_node_engine::exec::effect_node::EffectNode;
    use manifold_node_engine::ports::{ArrayType, PortType, ScalarType};

    use super::super::switch_mesh::tests::producers_run;
    use super::*;

    #[test]
    fn ports_are_a_param_shadowed_selector_and_two_index_inputs() {
        let node = SwitchIndices::new();
        let indices = PortType::Array(ArrayType::of_known::<u32>());
        let inputs: Vec<_> = EffectNode::inputs(&node)
            .iter()
            .map(|p| (p.name.as_ref(), p.ty, p.required))
            .collect();
        assert_eq!(
            inputs,
            [
                ("selector", PortType::Scalar(ScalarType::F32), true),
                ("in_0", indices, false),
                ("in_1", indices, false)
            ]
        );
        let outputs: Vec<_> = EffectNode::outputs(&node)
            .iter()
            .map(|p| (p.name.as_ref(), p.ty))
            .collect();
        assert_eq!(outputs, [("out", indices)]);
    }

    #[test]
    fn wired_selector_keeps_every_branch_and_a_param_picks_one() {
        let node = SwitchIndices::new();
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("selector"), ParamValue::Float(1.0));
        assert_eq!(
            Primitive::selected_input_branch(&node, &params, &["selector", "in_0", "in_1"]),
            None
        );
        assert_eq!(
            Primitive::selected_input_branch(&node, &params, &["in_0", "in_1"]),
            Some("in_1")
        );
        assert_eq!(
            Primitive::array_output_capacity(&node, "out", &params, &[("in_0", 9), ("in_1", 6)]),
            Some(9)
        );
    }

    #[test]
    fn param_selector_skips_the_producer_of_the_other_input() {
        let indices = PortType::Array(ArrayType::of_known::<u32>());
        assert_eq!(
            producers_run(Box::new(SwitchIndices::new()), indices, 1.0),
            ["b"]
        );
        assert_eq!(
            producers_run(Box::new(SwitchIndices::new()), indices, 0.0),
            ["a"]
        );
    }
}
