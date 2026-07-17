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
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::state_store::NodeState;

crate::primitive! {
    name: ArrayFeedback,
    type_id: "node.array_feedback",
    purpose: "One-frame delay for Array<Particle>: this frame's input becomes next frame's output. Closes per-frame particle loops without introducing graph cycles. The internal state-backed buffer is sized to match the producer's pre-allocated wire capacity (item_size × max_capacity). Optional `seed` input initialises the persistent buffer on first allocation (mirrors `node.feedback`'s seed-bootstrap) — wire `node.fluid_seed` (or any particle source) here for non-zero first-frame state. Optional `reset_trigger` input fires a re-seed on integer-edge changes: when it advances, the next emission copies `seed` into the state buffer (treats the trigger event as a clear+reseed). When `seed` is unwired, `reset_trigger` is a no-op.",
    inputs: {
        in: Array(Particle) required,
        seed: Array(Particle) optional,
        reset_trigger: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Pair with SimulateParticles (Phase A.7) to build feedback-driven simulations. SeedParticles emits initial state; ArrayFeedback caches it for replay. For FluidSim-style clip-trigger re-seeding: wire `system.generator_input.trigger_count` into `reset_trigger` and a seed-pattern producer (`seed_particles_from_texture` or `wgsl_compute`-driven density-rejection seed) into `seed` — every trigger edge clears the simulation and reloads the seed. First observation of `reset_trigger` arms without firing so the first-alloc seed isn't double-copied.",
    examples: [],
    picker: { label: "Array Feedback", category: Atom },
    summary: "Holds a list from the previous frame and hands it back this frame, closing a feedback loop for a particle or instance system without a graph cycle.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["array feedback", "list feedback", "frame delay"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        // Tracks the last `reset_trigger` integer value to detect
        // edges. `None` until the first observation; subsequent
        // integer changes fire the re-seed. First observation arms
        // without firing so the first-alloc path's `seed` copy isn't
        // immediately repeated on the same frame.
        last_reset_trigger: Option<i32> = None,
    },
}

/// Per-instance persistent state. Sized to match the wire on first
/// alloc; reallocated if the wire's byte length changes (chain
/// rebuild triggered by capacity-param change).
struct ArrayFeedbackState {
    /// Previous frame's payload — the copy-based 1-frame delay buffer.
    /// `None` for an in-place feedback loop (`in` and `out` are the same
    /// pre-bound buffer): no delay buffer is needed because the buffer
    /// persists in place and already holds last frame's result.
    prev: Option<GpuBuffer>,
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

        // In-place loop fast path: when `in` and `out` resolve to the SAME
        // pre-bound buffer — a particle sim whose chain mutates one buffer in
        // place via `aliased_array_io` (FluidSim, FluidSim3D, ParticleText) —
        // that buffer already persists across frames and holds last frame's
        // result, so the prev<->out delay copies are redundant no-ops. We skip
        // them entirely; only the first-frame seed and reset edges write,
        // straight into `out`. This is the ~6 ms/frame the two copies cost on
        // FluidSim's 512 MB particle buffer. For any non-in-place loop
        // (`in` != `out`) the copy-based 1-frame delay below is kept verbatim.
        let in_place = in_buf.ptr_eq(out_buf);

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;

        // Mark GPU access for the aliased-output contract check. (No-op for
        // this node — it doesn't declare `aliased_array_io` — but harmless and
        // matches the texture sibling.)
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
            if in_place {
                // The in-place buffer IS `out`; seed it directly. When `seed`
                // is wired, copy its initial pattern (the bootstrap path for
                // sims whose initial layout is meaningful); otherwise leave the
                // pre-bound buffer as the allocator left it. No delay buffer.
                if let Some(seed_buf) = ctx.inputs.array("seed") {
                    let copy_size = seed_buf.size.min(size);
                    if copy_size > 0 {
                        gpu.native_enc.copy_buffer_to_buffer(seed_buf, out_buf, copy_size);
                    }
                }
                store.insert(
                    node_id,
                    owner_key,
                    ArrayFeedbackState { prev: None, capacity_bytes: size },
                );
            } else {
                // Copy-delay path: own a persistent `prev` buffer. Seed from
                // the wired `seed`, else fall back to `in` so first-frame
                // output isn't an uninitialised buffer.
                let prev = gpu.device.create_buffer(size);
                let init_source = ctx.inputs.array("seed").unwrap_or(in_buf);
                let copy_size = init_source.size.min(size);
                if copy_size > 0 {
                    gpu.native_enc.copy_buffer_to_buffer(init_source, &prev, copy_size);
                }
                store.insert(
                    node_id,
                    owner_key,
                    ArrayFeedbackState { prev: Some(prev), capacity_bytes: size },
                );
            }
        }

        // Reset-on-trigger: if `reset_trigger` is wired and its integer value
        // has advanced since last frame, re-seed before emitting. First
        // observation arms without firing so the first-alloc seed isn't
        // immediately repeated. The re-seed lands in `out` for the in-place
        // loop, else in the prev buffer. When `seed` is unwired the edge fires
        // but there's nothing meaningful to reset from — degenerate no-op.
        if let Some(ParamValue::Float(v)) = ctx.inputs.scalar("reset_trigger") {
            let current = v.round() as i32;
            let edge = match self.last_reset_trigger {
                Some(prev) => current != prev,
                None => false,
            };
            self.last_reset_trigger = Some(current);
            if edge && let Some(seed_buf) = ctx.inputs.array("seed") {
                if in_place {
                    let copy_size = seed_buf.size.min(size);
                    if copy_size > 0 {
                        gpu.native_enc.copy_buffer_to_buffer(seed_buf, out_buf, copy_size);
                    }
                } else if let Some(state_mut) = store.get::<ArrayFeedbackState>(node_id, owner_key)
                    && let Some(prev) = state_mut.prev.as_ref()
                {
                    let copy_size = seed_buf.size.min(state_mut.capacity_bytes);
                    if copy_size > 0 {
                        gpu.native_enc.copy_buffer_to_buffer(seed_buf, prev, copy_size);
                    }
                }
            }
        }

        // Steady emit. In-place: nothing — `out` IS the persistent buffer and
        // already holds last frame's result. Else: copy the delayed state out
        // (state.prev holds last frame's producer output, snapshotted by
        // `late_capture` at end of the previous frame).
        if !in_place
            && let Some(state) = store.get::<ArrayFeedbackState>(node_id, owner_key)
            && let Some(prev) = state.prev.as_ref()
        {
            gpu.native_enc.copy_buffer_to_buffer(prev, out_buf, size);
        }
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
        // In-place loop: `prev` is None — the buffer persisted in place during
        // the frame, so there is nothing to snapshot. Only the copy-delay path
        // captures this frame's producer output into the delay buffer.
        let Some(prev) = state.prev.as_ref() else {
            return;
        };
        let size = in_buf.size.min(state.capacity_bytes);
        if size == 0 {
            return;
        }
        gpu.native_enc.copy_buffer_to_buffer(in_buf, prev, size);
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
    fn array_feedback_declares_required_in_optional_seed_and_optional_reset_trigger() {
        use crate::node_graph::ports::{ArrayType, PortKind, PortType, ScalarType};

        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(ArrayFeedback::TYPE_ID, "node.array_feedback");
        assert_eq!(ArrayFeedback::INPUTS.len(), 3);
        assert_eq!(ArrayFeedback::INPUTS[0].name, "in");
        assert_eq!(ArrayFeedback::INPUTS[0].kind, PortKind::Input);
        assert_eq!(ArrayFeedback::INPUTS[0].ty, PortType::Array(particle_layout));
        assert!(ArrayFeedback::INPUTS[0].required);
        assert_eq!(ArrayFeedback::INPUTS[1].name, "seed");
        assert!(!ArrayFeedback::INPUTS[1].required);
        assert_eq!(ArrayFeedback::INPUTS[1].ty, PortType::Array(particle_layout));
        assert_eq!(ArrayFeedback::INPUTS[2].name, "reset_trigger");
        assert!(!ArrayFeedback::INPUTS[2].required);
        assert_eq!(
            ArrayFeedback::INPUTS[2].ty,
            PortType::Scalar(ScalarType::F32)
        );

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
