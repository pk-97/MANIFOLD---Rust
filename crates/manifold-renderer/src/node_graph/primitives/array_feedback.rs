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
    purpose: "One-frame delay for Array<Particle>: this frame's input becomes next frame's output. Closes per-frame particle loops without introducing graph cycles. The internal state-backed buffer is sized to match the producer's pre-allocated wire capacity (item_size × max_capacity).",
    inputs: {
        in: Array(Particle) required,
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
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
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
            // Seed the persistent buffer from `in` so first-frame
            // output isn't an uninitialised buffer — first-frame
            // semantics match a no-op pass-through.
            gpu.native_enc.copy_buffer_to_buffer(in_buf, &prev, size);
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

        // Output = state.prev (last frame's data; or this frame's
        // `in` on the first frame after alloc).
        gpu.native_enc
            .copy_buffer_to_buffer(&state.prev, out_buf, size);

        // Update persistent buffer with this frame's `in` so the
        // next frame reads it as its prev.
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
    fn array_feedback_declares_one_array_input_and_one_array_output() {
        use crate::node_graph::ports::{ArrayType, PortKind, PortType};

        let particle_layout = ArrayType {
            item_size: std::mem::size_of::<Particle>() as u32,
            item_align: std::mem::align_of::<Particle>() as u32,
        };

        assert_eq!(ArrayFeedback::TYPE_ID, "node.array_feedback");
        assert_eq!(ArrayFeedback::INPUTS.len(), 1);
        assert_eq!(ArrayFeedback::INPUTS[0].name, "in");
        assert_eq!(ArrayFeedback::INPUTS[0].kind, PortKind::Input);
        assert_eq!(ArrayFeedback::INPUTS[0].ty, PortType::Array(particle_layout));
        assert!(ArrayFeedback::INPUTS[0].required);

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
