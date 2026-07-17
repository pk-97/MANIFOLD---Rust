//! `node.clip_trigger_cycle` — the §7 clip-trigger uniqueness
//! invariant as a graph primitive. Wraps
//! [`crate::generators::clip_trigger::ClipTriggerCycle::step`] —
//! emits `trigger_count % modulus`, but advances by +1 when the
//! candidate would equal the previous emission (defends against
//! host-counter glitches and clean modulus wraps that would
//! otherwise produce back-to-back duplicates).
//!
//! The canonical home for the "cycle through N outputs on each clip
//! retrigger" math. Use this instead of a bare `node.math` modulo
//! chain whenever the visual contract requires that consecutive
//! triggers never produce the same index — FluidSim2D's mode-3
//! re-seed (cycle the 7 seed patterns), Lissajous's harmonic ratio
//! row selection, NestedCubes's pose advance, etc.
//!
//! State: a single `ClipTriggerCycle` in the primitive's
//! `extra_fields`. Reset on graph rebuild (the §10 known limit —
//! graph editor is authoring, not performance).

use std::borrow::Cow;

use crate::generators::clip_trigger::ClipTriggerCycle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: ClipTriggerCycleNode,
    type_id: "node.clip_trigger_cycle",
    purpose: "Defense-in-depth `trigger_count % modulus` cycle: emits a value in [0, modulus) on each new trigger_count, advancing past would-be repeats so consecutive emissions never duplicate. Pair with `node.fluid_seed.pattern` for the legacy 7-pattern re-seed cycle, or any other discrete selector that must never fire the same row twice in a row. Pass RAW `trigger_count` (never pre-wrapped) — the cycle handles the modulus internally and pre-wrapping breaks idempotence.",
    inputs: {
        trigger_count: ScalarF32 required,
        modulus: ScalarF32 optional,
    },
    outputs: {
        out: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("modulus"),
            label: "Modulus",
            ty: ParamType::Int,
            default: ParamValue::Float(7.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "modulus = 7 matches FluidSim's seed-pattern cycle; other generators dial in their own row count. Both inputs are port-shadows-param. Idempotent within a frame: repeated calls at the same trigger_count return the cached emission.",
    examples: [],
    picker: { label: "Clip Trigger Cycle", category: Driver },
    summary: "Steps through a range on each clip trigger, never landing on the same value twice in a row. Drives never-repeat preset cycling.",
    category: Control,
    role: Control,
    aliases: ["clip trigger cycle", "cycle", "rotate"],
    boundary_reason: NonGpu,
    extra_fields: {
        cycle: ClipTriggerCycle = ClipTriggerCycle::new(),
    },
}

impl Primitive for ClipTriggerCycleNode {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let trigger_count = match ctx.inputs.scalar("trigger_count") {
            Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
            _ => return,
        };
        let modulus = match ctx.inputs.scalar("modulus") {
            Some(ParamValue::Float(f)) => f.round().max(1.0) as u32,
            _ => match ctx.params.get("modulus") {
                Some(ParamValue::Float(f)) => f.round().max(1.0) as u32,
                _ => 7,
            },
        };
        let index = self.cycle.step(trigger_count, modulus);
        ctx.outputs.set_scalar("out", ParamValue::Float(index as f32));
    }

    /// BUG-104: release the cycle's idempotence tracking so a fresh
    /// trigger after release re-arms cleanly instead of carrying a stale
    /// `last_emitted` forward forever. See `EffectNode::is_trigger_latch`.
    fn clear_state(&mut self) {
        self.cycle = ClipTriggerCycle::new();
    }

    fn is_trigger_latch(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_required_trigger_optional_modulus_and_one_scalar_out() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(ClipTriggerCycleNode::TYPE_ID, "node.clip_trigger_cycle");
        let inputs = ClipTriggerCycleNode::INPUTS;
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].name, "trigger_count");
        assert!(inputs[0].required);
        assert_eq!(inputs[0].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(inputs[1].name, "modulus");
        assert!(!inputs[1].required);
        let outputs = ClipTriggerCycleNode::OUTPUTS;
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].name, "out");
        assert_eq!(outputs[0].ty, PortType::Scalar(ScalarType::F32));
    }

    #[test]
    fn primitive_registers_as_palette_driver() {
        let prim = ClipTriggerCycleNode::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.clip_trigger_cycle");
    }

    /// Forwards directly to ClipTriggerCycle::step — these tests
    /// mirror the legacy ones to lock in the wrapper.
    #[test]
    fn sequential_trigger_counts_pass_through_unchanged() {
        let mut prim = ClipTriggerCycleNode::new();
        for i in 0..20u32 {
            let out = prim.cycle.step(i, 8);
            assert_eq!(out, i % 8);
        }
    }

    #[test]
    fn would_be_repeat_advances_by_one() {
        let mut prim = ClipTriggerCycleNode::new();
        assert_eq!(prim.cycle.step(5, 8), 5);
        assert_eq!(prim.cycle.step(13, 8), 6);
    }

    #[test]
    fn is_trigger_latch_flag_is_set() {
        let prim = ClipTriggerCycleNode::new();
        let node: &dyn EffectNode = &prim;
        assert!(node.is_trigger_latch());
    }

    /// BUG-104 — `clear_state()`, called through the same `EffectNode`
    /// trait object `PresetRuntime::clear_trigger_state` uses, releases
    /// the idempotence cache instead of leaving it latched on the last
    /// trigger_count forever.
    #[test]
    fn clear_state_releases_the_cycle_idempotence_cache() {
        let mut prim = ClipTriggerCycleNode::new();
        assert_eq!(prim.cycle.step(0, 8), 0);
        assert_eq!(prim.cycle.step(8, 8), 1); // would repeat 0 — advances
        assert_eq!(prim.cycle.step(8, 8), 1); // idempotent on same input

        {
            let node: &mut dyn EffectNode = &mut prim;
            node.clear_state();
        }

        assert_eq!(prim.cycle.step(8, 8), 0, "released cycle should re-arm to a fresh computation");
    }
}
