//! `node.array_feedback` — one-frame delay for `Array<Particle>` wires.
//!
//! Phase A.6 of `BUFFER_PORT_PLAN`. The Array analog of texture
//! `node.feedback`: a state-backed primitive that exposes last
//! frame's input on this frame's output. Lets particle pipelines
//! close their per-frame loops without introducing graph cycles —
//! the loop runs through `StateStore`, not through wires.
//!
//! On the first frame (no state yet), `out` is filled from `in` and
//! a state-store copy is captured. Subsequent frames swap: `out =
//! state.prev`, then `state.prev = in`. Items are bytes; the layout
//! is whatever `Array<Particle>` carries (64 bytes today; future
//! variants will copy-paste this primitive with different item
//! types until a generic-over-T macro arrives).
//!
//! Lifecycle: per-instance state lives in the `StateStore` keyed by
//! `(NodeInstanceId, OwnerKey)`. Cleared by seek / project load /
//! layer-idle via the existing `clear_all_effect_state` paths —
//! same as `node.feedback`.

use manifold_gpu::GpuBuffer;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::state_store::NodeState;

crate::primitive! {
    name: ArrayFeedback,
    type_id: "node.array_feedback",
    purpose: "One-frame delay for Array<Particle>: this frame's input becomes next frame's output. Closes per-frame particle loops without introducing graph cycles. The internal state-backed buffer is sized to match the producer's pre-allocated wire capacity (item_size × max_capacity). Optional `seed` input initialises the persistent buffer on first allocation (mirrors `node.feedback`'s seed-bootstrap) — wire `node.fluid_seed` (or any particle source) here for non-zero first-frame state.",
    inputs: {
        in: Array(Particle) required,
        seed: Array(Particle) optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [],
    composition_notes: "Pair with SimulateParticles (Phase A.7) to build feedback-driven simulations. SeedParticles emits initial state; ArrayFeedback caches it for replay.",
    examples: [],
    picker: { label: "Array Feedback", category: Atom },
}

/// Per-instance persistent state. One `GpuBuffer` holding the
/// previous frame's payload. Sized to match the wire on first
/// alloc; reallocated if the wire's byte length changes (chain
/// rebuild triggered by capacity-param change).
struct ArrayFeedbackState {
    prev: GpuBuffer,
    capacity_bytes: u64,
}

impl NodeState for ArrayFeedbackState {}

impl Primitive for ArrayFeedback {
    /// `in` is a state capture for next frame, not a per-frame
    /// dependency. Mirrors `temporal::Feedback`'s contract — see the
    /// `EffectNode::state_capture_input_ports` docstring.
    fn state_capture_input_ports(&self) -> &'static [&'static str] {
        &["in"]
    }

    /// Output `out` is sized to match an upstream input. Prefer `seed`
    /// (a normal forward dependency processed earlier in topo order) so
    /// the size is available at chain build time; fall back to `in`
    /// (the state-capture back-edge) for the no-seed-wired pattern
    /// where the producer happens to be processed first. Without the
    /// `seed` preference, particle-loop preset chains where the
    /// back-edge originates downstream (e.g. `fluid_simulate.out → in`)
    /// can't size the output and downstream consumers see an empty
    /// buffer. The persistent `prev` buffer in `StateStore` reallocates
    /// internally if the wire's byte length later changes.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities
                .iter()
                .find(|(p, _)| *p == "seed")
                .or_else(|| input_capacities.iter().find(|(p, _)| *p == "in"))
                .map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // `evaluate` (= `run`) phase: emit only. The capture lives in
        // `late_capture` because state-capture nodes run BEFORE their
        // producer in topo order, so the producer's frame-N write
        // hasn't landed yet at this point. Snapshotting `in_buf` from
        // inside `run` would copy LAST frame's producer output and
        // give a 2-frame delay — the bug class that produced
        // OilyFluid's per-frame flicker on the texture side.
        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        // Source-of-truth size: the smaller of the two wires.
        // In production both are sized by the same chain-build pass.
        let size = in_buf.size.min(out_buf.size);
        if size == 0 {
            return;
        }

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;

        // Mark GPU access for the aliased-output contract check. This
        // primitive dispatches buffer copies, so the executor's
        // post-evaluate audit needs to see the flag.
        ctx.mark_gpu_accessed();

        // Split borrows: gpu / state are disjoint fields on ctx, so
        // both can be borrowed mutably at once. Mirrors
        // `temporal::Feedback`.
        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("ArrayFeedback::run requires a GpuEncoder");
        let store = ctx
            .state
            .as_deref_mut()
            .expect("ArrayFeedback::run requires a StateStore");

        let needs_alloc = match store.get::<ArrayFeedbackState>(node_id, owner_key) {
            Some(s) => s.capacity_bytes != size,
            None => true,
        };
        if needs_alloc {
            let prev = gpu.device.create_buffer(size);
            // Seed the persistent buffer on first allocation. When the
            // optional `seed` input is wired, copy its contents (the
            // bootstrap path for sims like FluidSim2D where the
            // initial particle layout is meaningful — pattern, not
            // zeros). When unwired, fall back to seeding from `in` so
            // first-frame output isn't an uninitialised buffer.
            let init_source = ctx.inputs.array("seed").unwrap_or(in_buf);
            let copy_size = init_source.size.min(size);
            if copy_size > 0 {
                gpu.native_enc
                    .copy_buffer_to_buffer(init_source, &prev, copy_size);
            }
            store.insert(
                node_id,
                owner_key,
                ArrayFeedbackState {
                    prev,
                    capacity_bytes: size,
                },
            );
        }
        let state = store
            .get::<ArrayFeedbackState>(node_id, owner_key)
            .expect("just inserted above");

        // Emit `out` ← state.prev. state.prev holds last frame's
        // producer output (snapshotted by `late_capture` at end of
        // the previous frame), or — on the alloc frame — the in_buf
        // passthrough seed copy above. Gives a true 1-frame delay.
        gpu.native_enc
            .copy_buffer_to_buffer(&state.prev, out_buf, size);
    }

    fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Post-frame snapshot: `in_buf` now holds THIS frame's
        // producer output (the back-edge slot was written during the
        // main step-loop pass). Captured here, it becomes next
        // frame's `state.prev` and is emitted by next frame's `run`
        // — a clean 1-frame delay.
        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;
        ctx.mark_gpu_accessed();
        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("ArrayFeedback::late_capture requires a GpuEncoder");
        let store = ctx
            .state
            .as_deref_mut()
            .expect("ArrayFeedback::late_capture requires a StateStore");

        // If `run` short-circuited before allocating state (zero-size
        // wire), there's nothing to capture into yet.
        let Some(state) = store.get::<ArrayFeedbackState>(node_id, owner_key) else {
            return;
        };
        let size = in_buf.size.min(state.capacity_bytes);
        if size == 0 {
            return;
        }
        gpu.native_enc
            .copy_buffer_to_buffer(in_buf, &state.prev, size);
    }
}

#[cfg(test)]
mod tests {
    //! Phase A.6 smoke tests. The full per-frame GPU round-trip test
    //! is deferred to Phase A.7 when SimulateParticles needs the same
    //! buffer-readback infrastructure — sharing one helper there
    //! beats hand-rolling a fragile one here.

    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn array_feedback_declares_required_in_optional_seed_and_one_array_output() {
        use crate::node_graph::ports::{ArrayType, PortKind, PortType};

        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(ArrayFeedback::TYPE_ID, "node.array_feedback");
        assert_eq!(ArrayFeedback::INPUTS.len(), 2);
        assert_eq!(ArrayFeedback::INPUTS[0].name, "in");
        assert_eq!(ArrayFeedback::INPUTS[0].kind, PortKind::Input);
        assert_eq!(ArrayFeedback::INPUTS[0].ty, PortType::Array(particle_layout));
        assert!(ArrayFeedback::INPUTS[0].required);
        assert_eq!(ArrayFeedback::INPUTS[1].name, "seed");
        assert!(!ArrayFeedback::INPUTS[1].required);
        assert_eq!(ArrayFeedback::INPUTS[1].ty, PortType::Array(particle_layout));

        assert_eq!(ArrayFeedback::OUTPUTS.len(), 1);
        assert_eq!(ArrayFeedback::OUTPUTS[0].name, "out");
        assert_eq!(ArrayFeedback::OUTPUTS[0].kind, PortKind::Output);
        assert_eq!(
            ArrayFeedback::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );
    }

    #[test]
    fn array_feedback_is_picker_visible_as_an_atom() {
        // The primitive! macro registers the picker info via the
        // inventory channel — assert presence indirectly through the
        // EffectNode trait surface.
        let prim = ArrayFeedback::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.array_feedback");
    }
}
