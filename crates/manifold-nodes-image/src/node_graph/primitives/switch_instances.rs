//! `node.switch_instances` — two-way `Array<InstanceTransform>` selector: the
//! instance-array twin of `node.switch_mesh`, which owns the shared
//! passthrough and run path ([`ArraySwitch`]).

use std::borrow::Cow;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::mesh::InstanceTransform;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

use super::switch_mesh::{
    ArraySwitch, selected_switch_input, switch_output_capacity, switch_passthrough,
};

manifold_node_engine::primitive! {
    name: SwitchInstances,
    type_id: "node.switch_instances",
    purpose: "Two-way Array<InstanceTransform> selector. Passes in_0 or in_1 through to out with the chosen input's live extent, chosen by the selector (rounded, clamped). With the selector set as a param, out is the chosen input's own buffer for the frame (no copy, no GPU work) and everything feeding only the other input is skipped; a wired selector blits the chosen live instances on the GPU. Output capacity = the larger wired input.",
    inputs: {
        selector: ScalarF32 required,
        in_0: Array(InstanceTransform) optional,
        in_1: Array(InstanceTransform) optional,
    },
    outputs: {
        out: Array(InstanceTransform),
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
    composition_notes: "Wire into a node.scene_object's instances to stop the copies feeding it when they are not shown: set the selector as a param (port-shadowed: a wired selector keeps both branches running) and leave the other input unwired. An unwired choice is all-zero instances, which draw nothing, and its producer chain is skipped.",
    examples: [],
    picker: { label: "Switch (instances)", category: Atom },
    summary: "Picks one of two incoming instance lists and passes it through, chosen by a selector number. The list it does not pick is not computed.",
    category: Routing,
    role: Filter,
    aliases: ["switch", "mux", "instance switch", "selector"],
    boundary_reason: NonGpu,
    extra_fields: {
        switch: ArraySwitch = ArraySwitch::default(),
    },
}

manifold_node_engine::param_tooltips!("node.switch_instances", {
    "selector" => "Picks which instance list to pass through: 0 or 1.",
});

impl Primitive for SwitchInstances {
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
            std::mem::size_of::<InstanceTransform>() as u64,
            "node.switch_instances",
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
    fn ports_are_a_param_shadowed_selector_and_two_instance_inputs() {
        let node = SwitchInstances::new();
        let instances = PortType::Array(ArrayType::of_known::<InstanceTransform>());
        let inputs: Vec<_> = EffectNode::inputs(&node)
            .iter()
            .map(|p| (p.name.as_ref(), p.ty, p.required))
            .collect();
        assert_eq!(
            inputs,
            [
                ("selector", PortType::Scalar(ScalarType::F32), true),
                ("in_0", instances, false),
                ("in_1", instances, false)
            ]
        );
        let outputs: Vec<_> = EffectNode::outputs(&node)
            .iter()
            .map(|p| (p.name.as_ref(), p.ty))
            .collect();
        assert_eq!(outputs, [("out", instances)]);
    }

    #[test]
    fn wired_selector_keeps_every_branch_and_a_param_picks_one() {
        let node = SwitchInstances::new();
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
            Primitive::array_output_capacity(&node, "out", &params, &[("in_1", 7)]),
            Some(7)
        );
    }

    #[test]
    fn param_selector_skips_the_producer_of_the_other_input() {
        let instances = PortType::Array(ArrayType::of_known::<InstanceTransform>());
        assert_eq!(
            producers_run(Box::new(SwitchInstances::new()), instances, 1.0),
            ["b"]
        );
        assert_eq!(
            producers_run(Box::new(SwitchInstances::new()), instances, 0.0),
            ["a"]
        );
    }
}
