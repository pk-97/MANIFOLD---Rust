//! `node.clip_trigger_index` — emit `trigger_count % modulus` as a
//! scalar, via the idempotence-safe [`ClipTriggerCycle`] gate.
//!
//! The control-rate cycling primitive for graph-level "swap to next
//! on clip retrigger" UX. The standalone counterpart to Plasma's
//! internal `clip_trigger_cycle.step(...)` — moved out into the
//! graph so any consumer (a mux_texture's selector, an Int param on
//! some downstream primitive) can drive its discrete behaviour off
//! the same clip-trigger source.
//!
//! Pass raw `trigger_count` in — never pre-wrap via `node.math`. The
//! gate's idempotence detection compares against the last raw value
//! it saw; pre-wrapping (`trigger_count % N` upstream) breaks the
//! comparison and reintroduces the 67f8db94 "identical back-to-back
//! emissions" bug.

use std::borrow::Cow;

use manifold_node_engine::clip_trigger::ClipTriggerCycle;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

manifold_node_engine::primitive! {
    name: ClipTriggerIndex,
    type_id: "node.clip_trigger_index",
    purpose: "Emit `trigger_count % modulus` as a scalar via the idempotence-safe ClipTriggerCycle gate. The graph-level counterpart to a primitive's internal clip-trigger cycling (Plasma does this inline). Use as `mux_texture.selector` to swap between N upstream variants on each clip retrigger; pair with `mux_scalar` and a static-axis param to support both manual and trigger-driven modes from one outer-card toggle.",
    inputs: {
        trigger_count: ScalarF32 optional,
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
            default: ParamValue::Float(3.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire system.generator_input.trigger_count → trigger_count input. Modulus is the cycle length (3 for a tri-state axis swap, 8 for an eight-variant pattern bank). Pass raw trigger_count — never pre-wrap via math.modulo upstream; the gate's idempotence detection compares against the last raw value it saw, so pre-wrapping breaks the cycle (the 67f8db94 bug class). Output is `current_index` as f32 in [0, modulus). State is preserved across frames inside the primitive's extra_fields and resets on generator rebuild — accepted authoring-time trade-off per section 10.",
    examples: [],
    picker: { label: "Clip Trigger Index", category: Atom },
    summary: "Counts how many times a clip has been triggered and wraps it to a range, so each retrigger steps to the next slot. Drives preset cycling.",
    category: Control,
    role: Control,
    aliases: ["clip trigger index", "trigger count", "cycle index"],
    boundary_reason: NonGpu,
    extra_fields: {
        cycle: ClipTriggerCycle = ClipTriggerCycle::new(),
    },
}

impl Primitive for ClipTriggerIndex {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let trigger_count = ctx.scalar_or_param("trigger_count", 0.0);
        let raw = trigger_count.floor().max(0.0) as u32;
        // Port-shadows-param: a wired `modulus` scalar overrides the
        // inline param every frame, letting a fill-mode mux drive the
        // cycle length (BasicShapes feeds a 3/6/3 mux based on fill).
        let modulus = ctx.scalar_or_param("modulus", 3.0).round().max(1.0) as u32;
        let idx = self.cycle.step(raw, modulus);
        ctx.outputs
            .set_scalar("out", ParamValue::Float(idx as f32));
    }

    /// BUG-104: release the cycle's idempotence tracking. See
    /// `EffectNode::is_trigger_latch`.
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
    use manifold_node_engine::exec::effect_node::EffectNode;

    /// BUG-104 — see `frequency_ratio`'s equivalent test for the full
    /// rationale; `clear_state()` releases the idempotence cache through
    /// the same `EffectNode` trait object `PresetRuntime::
    /// clear_trigger_state` uses.
    #[test]
    fn clear_state_releases_the_cycle_idempotence_cache() {
        let mut prim = ClipTriggerIndex::new();
        assert_eq!(prim.cycle.step(0, 3), 0);
        assert_eq!(prim.cycle.step(3, 3), 1); // would repeat 0 — advances
        assert_eq!(prim.cycle.step(3, 3), 1); // idempotent on same input

        {
            let node: &mut dyn EffectNode = &mut prim;
            node.clear_state();
        }

        assert_eq!(prim.cycle.step(3, 3), 0, "released cycle should re-arm to a fresh computation");
    }
}
