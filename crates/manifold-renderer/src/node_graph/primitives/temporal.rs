//! Temporal primitives — operations that maintain state across frames.
//!
//! V1 set: [`Feedback`].
//!
//! Temporal primitives are the first stateful nodes in the catalog. Their
//! state lives in the runtime's `StateStore`, keyed by
//! `(NodeInstanceId, OwnerKey)`, **not** in the node itself. This is the
//! pattern every future stateful primitive (frame difference, motion
//! blur, accumulators) follows.

use manifold_gpu::GpuTextureFormat;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::state_store::NodeState;
use crate::render_target::RenderTarget;

// =====================================================================
// Feedback — pure 1-frame texture delay. This frame's `in` becomes next
// frame's `out`. The texture analog of `node.array_feedback`.
//
// Closes per-frame feedback loops without introducing graph cycles:
// downstream nodes consume `out` (last frame's input) and the loop
// runs through the StateStore rather than through wires. Compose with
// `node.affine_transform` / `node.gain` / `node.mix` / `node.vignette`
// to build classic stylized-feedback chains, or with custom WGSL
// compute steps to build reaction-diffusion / fluid / paint sims.
// =====================================================================

pub const FEEDBACK_TYPE_ID: &str = "node.feedback";

crate::primitive! {
    name: Feedback,
    type_id: "node.feedback",
    purpose: "Pure 1-frame texture delay. This frame's `in` becomes next frame's `out`. Closes per-frame feedback loops without introducing graph cycles — the loop runs through the StateStore, not through wires. Compose with affine_transform + gain + mix + vignette for stylized-feedback chains, or with custom compute steps for fluid / reaction-diffusion sims.",
    inputs: {
        in: Texture2D required,
        seed: Texture2D optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [],
    composition_notes: "Wire the loop's final output back into `in`, and read `out` upstream as the previous frame. State is per-`(NodeInstanceId, OwnerKey)` so multiple layers / clips using the same chain get independent feedback streams. First-frame semantics: when `seed` is unwired, `out` mirrors `in` for one frame (no uninitialised pixels). When `seed` IS wired, the persistent state texture is initialised with the seed's contents on first allocation — use for sims that need a non-black initial state (oily fluid's layered noise seed, reaction-diffusion's spike pattern, etc.). The seed producer runs every frame in v1 but only matters on the first allocation; gating it to first-frame-only is a planner-pass follow-up. For iterative simulations whose state compounds rounding error, set `outputFormats.out: \"rgba32float\"` in the JSON node entry — note the loop's INTERMEDIATE producers (mix, gain, etc.) must also be annotated fp32 or Metal's blit will validation-error on the format-mismatched capture; defaulting to rgba16float for memory parity with the rest of the chain until that propagation lands.",
    examples: ["preset.effect.stylized_feedback", "preset.effect.mandala", "preset.effect.smear_mosh"],
    picker: { label: "Feedback", category: Atom },
    extra_fields: {
        output_format_override: Option<GpuTextureFormat> = None,
    },
}

const FEEDBACK_DEFAULT_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

/// Per-`(NodeInstanceId, OwnerKey)` persistent state — the previous
/// frame's input. Held by the runtime's `StateStore`.
struct FeedbackState {
    prev: RenderTarget,
    width: u32,
    height: u32,
}

impl NodeState for FeedbackState {}

impl Primitive for Feedback {
    fn output_format(&self, port: &str) -> Option<GpuTextureFormat> {
        if port == "out" {
            self.output_format_override
        } else {
            None
        }
    }

    fn set_output_format(&mut self, port: &str, format: GpuTextureFormat) {
        if port == "out" {
            self.output_format_override = Some(format);
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        // Optional seed for first-allocation init. When unwired, fall
        // back to seeding from `in` so first-frame `out` is a
        // pass-through (preserves the contract every existing
        // feedback-using preset relies on). When wired, the seed's
        // contents land in `state.prev` and downstream consumers see
        // a non-black initial state — used for sims that need
        // structured noise to start (oily fluid).
        let seed_tex = ctx.inputs.texture_2d("seed");
        let (width, height) = (out_tex.width, out_tex.height);
        if width == 0 || height == 0 {
            return;
        }

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;
        // Single source of truth for the state-texture format. The
        // executor's persistent-slot acquisition already honours
        // `EffectNode::output_format("out")` via the plan's
        // resource_format table — so `out_tex` is allocated in the
        // overridden format. `state.prev` MUST match: it's the source
        // for `out_tex`'s per-frame copy, and any format mismatch
        // would either quantize on the copy (fp32 → fp16) or violate
        // the texture-copy size invariant. Reading the override here
        // keeps the two allocations bit-aligned.
        let state_format = self.output_format_override.unwrap_or(FEEDBACK_DEFAULT_FORMAT);

        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("Feedback::run requires a GpuEncoder");
        let store = ctx
            .state
            .as_deref_mut()
            .expect("Feedback::run requires a StateStore");

        // Lazy-init the persistent prev-frame buffer. First-allocation
        // seed: when `seed` is wired, copy it in; otherwise fall back
        // to seeding from `in` (matches `node.array_feedback`'s
        // first-frame contract — first frame's `out` reads the
        // current input). Re-allocated if dims change.
        let needs_alloc = match store.get::<FeedbackState>(node_id, owner_key) {
            Some(s) => s.width != width || s.height != height,
            None => true,
        };
        if needs_alloc {
            let prev = if let Some(pool) = gpu.pool {
                RenderTarget::new_pooled(pool, width, height, state_format, "feedback prev")
            } else {
                RenderTarget::new(gpu.device, width, height, state_format, "feedback prev")
            };
            let init_source = seed_tex.unwrap_or(in_tex);
            gpu.copy_texture_to_texture(init_source, &prev.texture, width, height);
            store.insert(
                node_id,
                owner_key,
                FeedbackState {
                    prev,
                    width,
                    height,
                },
            );
        }
        let state = store
            .get::<FeedbackState>(node_id, owner_key)
            .expect("just inserted above");

        // Emit last frame's input as this frame's `out`.
        gpu.copy_texture_to_texture(&state.prev.texture, out_tex, width, height);
        // Capture this frame's `in` for next frame's `out` — but NOT
        // on the same frame as a fresh allocation. The in_slot at that
        // point is whatever the executor pre-cleared / left stale BEFORE
        // the chain's producer ran (feedback runs first under the
        // state-capture topo exemption, so the producer's write for this
        // frame hasn't landed yet). Capturing it would clobber the seed
        // we just installed into `state.prev`, and the very next frame's
        // `out = prev` would resolve to that zero — turning a seeded
        // chain into a 2-frame oscillation between "seed" and "zero"
        // instead of the intended bootstrap-then-evolve.
        //
        // Skipping the capture for one frame lets prev = seed survive
        // into frame 2. By frame 2's evaluation the chain has run once
        // with seed-driven `out`, so the slot now holds a meaningful
        // value and the capture resumes as normal. For the unseeded
        // case this is behaviorally identical: prev = in_tex = 0 from
        // the alloc init, and the skipped copy would have written 0
        // over 0 anyway.
        if !needs_alloc {
            gpu.copy_texture_to_texture(in_tex, &state.prev.texture, width, height);
        }
    }

    fn requires(&self) -> crate::node_graph::effect_node::NodeRequires {
        // Feedback emits texture copies (needs a GpuEncoder) and keys
        // its prev-frame buffer in the StateStore. A graph containing
        // this primitive must be run via `execute_frame_with_state`.
        crate::node_graph::effect_node::NodeRequires {
            state_store: true,
            gpu_encoder: true,
        }
    }

    fn state_capture_input_ports(&self) -> &'static [&'static str] {
        // `in` is a state capture for next frame, not a per-frame
        // dependency. Lets feedback chains like
        // `source → ... → mix → feedback → ... → mix` close their
        // loop in the wire graph instead of inside the primitive.
        //
        // `seed` is deliberately NOT listed: it's a one-shot init
        // source that has to run BEFORE this node on the first frame.
        // If it were listed here, the planner would pre-clear its
        // persistent slot to black and the seed would init from
        // garbage instead of the producer's actual output.
        &["in"]
    }
}

#[cfg(test)]
mod gpu_tests {
    //! Real-GPU regression test guarding the StateStore contract:
    //! dispatching `node.feedback` through an `Executor` requires a
    //! `StateStore` + `OwnerKey` to be plumbed through. Earlier
    //! `ChainGraph::run` used `execute_frame_with_gpu` which passes
    //! neither — Feedback would panic with
    //! "Feedback::run requires a StateStore" the moment any chain
    //! included it. The fix routes ChainGraph through
    //! `execute_frame_with_state`; this test locks the end-to-end path
    //! by running a minimal Source → Feedback → FinalOutput graph
    //! through it across two frames (the first allocates + seeds prev;
    //! the second actually reads it).

    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::{
        ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, MetalBackend, NodeInstanceId,
        ResourceId, Source, StateStore, compile,
    };
    use crate::render_target::RenderTarget;

    use super::Feedback;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn output_resource(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
        for step in plan.steps() {
            if step.node == node {
                for &(name, id) in &step.outputs {
                    if name == port {
                        return id;
                    }
                }
            }
        }
        panic!("no output `{port}` on node {node:?}");
    }

    #[test]
    fn feedback_dispatches_through_state_store_without_panic() {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let fb = g.add_node(Box::new(Feedback::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect((src, "out"), (fb, "in")).unwrap();
        g.connect((fb, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let source_res = output_resource(&plan, src, "out");
        let source_target = RenderTarget::new(&device, w, h, format, "test-feedback-src");
        let mut native_enc = device.create_encoder("feedback-smoke");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            gpu.clear_texture(&source_target.texture, 0.5, 0.5, 0.5, 1.0);
        }

        let mut backend = MetalBackend::new(&device, w, h, format);
        backend.pre_bind_texture_2d(source_res, source_target);

        let mut exec = Executor::new(Box::new(backend));
        let mut store = StateStore::new();
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_state(
                &mut g,
                &plan,
                frame_time(),
                &mut gpu,
                &mut store,
                /* owner_key = */ 7,
            );
        }
        native_enc.commit_and_wait_completed();

        // Second frame exercises the read path (first frame allocates
        // + seeds the prev buffer).
        let mut native_enc = device.create_encoder("feedback-smoke-2");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_state(&mut g, &plan, frame_time(), &mut gpu, &mut store, 7);
        }
        native_enc.commit_and_wait_completed();
    }

    /// `node.feedback`'s format override propagates from the
    /// per-instance setter all the way through to the compiled plan's
    /// `resource_format` table — so the executor's persistent-slot
    /// acquisition allocates in the requested format and the
    /// state-prev allocation matches. Locks the fp32-state contract
    /// that iterative simulations (oily fluid, future reaction-
    /// diffusion / SPH primitives) rely on for cross-frame precision.
    #[test]
    fn feedback_output_format_override_propagates_to_plan_and_state() {
        use crate::node_graph::EffectNode;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let fb = g.add_node(Box::new(Feedback::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect((src, "out"), (fb, "in")).unwrap();
        g.connect((fb, "out"), (out, "in")).unwrap();

        // Default: no override → output_format("out") = None → planner
        // records None → backend acquires in its constructor default.
        let plan_default = compile(&g).unwrap();
        let fb_out = output_resource(&plan_default, fb, "out");
        assert_eq!(plan_default.resource_format(fb_out), None);

        // Apply the override via the public Graph setter (same path
        // EffectGraphDef::into_graph uses when loading a JSON node
        // with `outputFormats: { "out": "rgba32float" }`).
        g.set_output_format(fb, "out", GpuTextureFormat::Rgba32Float)
            .unwrap();

        // Re-query through the trait surface — proves the setter
        // wrote to the per-instance override, not just a local copy.
        let inst = g.get_node(fb).unwrap();
        let node: &dyn EffectNode = inst.node.as_ref();
        assert_eq!(node.output_format("out"), Some(GpuTextureFormat::Rgba32Float));
        // Sibling ports unaffected.
        assert_eq!(node.output_format("nonexistent"), None);

        // Recompile picks up the new format and threads it onto
        // feedback.out's resource. The executor's step loop reads
        // this via `plan.resource_format` when acquiring the output
        // slot (execution.rs:211), so downstream consumers of
        // feedback.out see the fp32 storage. Note: feedback.out itself
        // is NOT in `persistent_resources` — persistent resources are
        // the wires whose CONSUMER is a cycle-breaker (state-capture
        // sources). Feedback's `out` is acquired fresh each frame; the
        // cross-frame data lives in the StateStore-owned `state.prev`
        // and (for graphs that close a loop through feedback) in the
        // persistent slot for the wire entering feedback.in.
        let plan = compile(&g).unwrap();
        let fb_out = output_resource(&plan, fb, "out");
        assert_eq!(
            plan.resource_format(fb_out),
            Some(GpuTextureFormat::Rgba32Float),
            "fp32 override must reach plan.resource_format so the executor's \
             backend.acquire allocates feedback.out's slot at the requested precision",
        );
    }

    /// First-frame allocation of the StateStore-owned `state.prev`
    /// texture honours the format override. Without the override read
    /// in `run()`, `state.prev` would stay rgba16float while the
    /// executor-allocated `out_tex` upcasts to rgba32float — and the
    /// per-frame `copy_texture_to_texture(state.prev → out_tex)`
    /// would silently quantize / mismatch.
    ///
    /// Contract being asserted: when you override feedback.out's format,
    /// the writer feeding feedback.in MUST be overridden to the same
    /// format. Otherwise the per-frame `copy_texture_to_texture(in_tex
    /// → state.prev)` is a format mismatch and Metal's blit encoder
    /// either faults loudly (debug validation) or silently corrupts
    /// (release). The OilyFluid breakage was exactly this — fp32 state
    /// override on `node.feedback` but fp16 default on the writer chain
    /// (`vel_combine`, `color_combine`). The fix at the manifold-gpu
    /// layer (assertions in `copy_texture_to_texture`) means this is
    /// now a one-frame panic at the offending call site rather than
    /// hours of "stays static" debugging.
    #[test]
    fn feedback_run_allocates_state_prev_in_overridden_format() {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let fb = g.add_node(Box::new(Feedback::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect((src, "out"), (fb, "in")).unwrap();
        g.connect((fb, "out"), (out, "in")).unwrap();
        // Both the writer feeding feedback.in AND feedback.out itself
        // need fp32 — otherwise the per-frame in_tex → state.prev copy
        // is a format mismatch. See the docstring above.
        g.set_output_format(src, "out", GpuTextureFormat::Rgba32Float)
            .unwrap();
        g.set_output_format(fb, "out", GpuTextureFormat::Rgba32Float)
            .unwrap();
        let plan = compile(&g).unwrap();

        // Seed the source slot at fp32 to match the override.
        let source_res = output_resource(&plan, src, "out");
        let source_target =
            RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba32Float, "fp32-feedback-src");
        let mut native_enc = device.create_encoder("fp32-feedback");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            gpu.clear_texture(&source_target.texture, 0.25, 0.5, 0.75, 1.0);
        }

        // Build the backend at rgba16float (the chain default). The
        // backend slot pool keys on (PortType, GpuTextureFormat), so
        // the feedback persistent slot opens in a separate fp32 bucket
        // without colliding with regular 16f slots.
        let mut backend =
            MetalBackend::new(&device, w, h, GpuTextureFormat::Rgba16Float);
        backend.pre_bind_texture_2d(source_res, source_target);

        let mut exec = Executor::new(Box::new(backend));
        let mut store = StateStore::new();
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            // First frame: allocates state.prev. If `run()` didn't
            // read the override the copy on the next frame would
            // either crash (format mismatch) or silently quantize.
            exec.execute_frame_with_state(
                &mut g,
                &plan,
                frame_time(),
                &mut gpu,
                &mut store,
                /* owner_key = */ 11,
            );
        }
        native_enc.commit_and_wait_completed();

        // Second frame would fault inside `copy_texture_to_texture`
        // if state.prev and out_tex disagreed on format. Surviving the
        // round-trip is the assertion.
        let mut native_enc = device.create_encoder("fp32-feedback-2");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_state(
                &mut g,
                &plan,
                frame_time(),
                &mut gpu,
                &mut store,
                11,
            );
        }
        native_enc.commit_and_wait_completed();
    }

    /// The seed-bootstrap contract: when `seed` is wired, the seed
    /// value MUST survive into frame 2's output, not just frame 1.
    /// Without this, the dual-buffer (state.prev + per-frame capture)
    /// pattern destroys the seed at the end of frame 1 — capturing the
    /// in_slot's pre-cleared zero into state.prev, and frame 2 emits
    /// that zero. The chain alternates seed/zero/seed/zero and never
    /// bootstraps. Regression-locks the OilyFluid breakage where the
    /// fluid sim never built up from the noise seed.
    ///
    /// Strategy: build `noise_src → feedback.seed`, leave feedback.in
    /// unwired (it'll fall through to a black persistent slot, mimicking
    /// the pre-chain-write state of a real feedback loop's first frame).
    /// Run two frames. Read back out_slot at end of frame 2. Expect the
    /// seed's color, not zero.
    #[test]
    fn feedback_seed_survives_into_second_frame() {
        use crate::node_graph::Backend;
        use crate::node_graph::bindings::Slot;
        use half::f16;

        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        // Graph: seed_src → feedback.seed, feedback.out → final.
        // feedback.in is unwired; the runtime treats unwired inputs as
        // absent and the producer falls back to the seed for first-alloc
        // init (`init_source = seed_tex.unwrap_or(in_tex)`). To exercise
        // the "in_tex slot is pre-cleared / stale" condition we still
        // wire feedback.in — but to a different source whose slot we
        // pre-bind to BLACK so the capture-into-prev would clobber the
        // seed if the skip-on-fresh-alloc fix regressed.
        let mut g = Graph::new();
        let in_src = g.add_node(Box::new(Source::new()));
        let seed_src = g.add_node(Box::new(Source::new()));
        let fb = g.add_node(Box::new(Feedback::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect((in_src, "out"), (fb, "in")).unwrap();
        g.connect((seed_src, "out"), (fb, "seed")).unwrap();
        g.connect((fb, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let in_res = output_resource(&plan, in_src, "out");
        let seed_res = output_resource(&plan, seed_src, "out");

        // Pre-bind the in_src slot to BLACK and the seed_src slot to a
        // distinctive color (0.7, 0.3, 0.1). Frame 2's output is the
        // assertion: if the skip-on-fresh-alloc fix is in place, the
        // seed color makes it through; without the fix, in_src's black
        // overwrites prev at end of frame 1 and frame 2 emits black.
        let in_target = RenderTarget::new(&device, w, h, format, "test-in-black");
        let seed_target = RenderTarget::new(&device, w, h, format, "test-seed-color");
        let mut native_enc = device.create_encoder("seed-fill");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            gpu.clear_texture(&in_target.texture, 0.0, 0.0, 0.0, 1.0);
            gpu.clear_texture(&seed_target.texture, 0.7, 0.3, 0.1, 1.0);
        }

        let mut backend = MetalBackend::new(&device, w, h, format);
        backend.pre_bind_texture_2d(in_res, in_target);
        backend.pre_bind_texture_2d(seed_res, seed_target);
        let out_slot = Slot(backend.slot_count());

        let mut exec = Executor::new(Box::new(backend));
        let mut store = StateStore::new();

        // Frame 1: allocates, seeds, emits seed.
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_state(
                &mut g,
                &plan,
                frame_time(),
                &mut gpu,
                &mut store,
                17,
            );
        }
        native_enc.commit_and_wait_completed();

        // Frame 2: no alloc. With the skip-on-fresh-alloc fix in place,
        // state.prev still = seed (the capture was skipped on frame 1),
        // so frame 2's `out := prev` again emits the seed color. Without
        // the fix, state.prev = in_tex = black (captured at end of frame
        // 1), and frame 2's output would be black.
        let mut native_enc = device.create_encoder("seed-bootstrap-frame-2");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_state(
                &mut g,
                &plan,
                frame_time(),
                &mut gpu,
                &mut store,
                17,
            );
        }
        native_enc.commit_and_wait_completed();

        let out_tex = exec
            .backend()
            .texture_2d(out_slot)
            .expect("output texture retained");
        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("seed-readback");
        readback_enc.copy_texture_to_buffer(out_tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();
        let ptr = readback_buf.mapped_ptr().expect("shared buffer pointer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        let pixel = [
            f16::from_bits(halves[0]).to_f32(),
            f16::from_bits(halves[1]).to_f32(),
            f16::from_bits(halves[2]).to_f32(),
            f16::from_bits(halves[3]).to_f32(),
        ];
        let tol = 0.01;
        assert!(
            (pixel[0] - 0.7).abs() < tol
                && (pixel[1] - 0.3).abs() < tol
                && (pixel[2] - 0.1).abs() < tol,
            "frame-2 output must still be the seed color (0.7, 0.3, 0.1) — \
             skip-on-fresh-alloc keeps state.prev = seed through frame 2 so \
             the chain bootstraps from real content instead of alternating \
             between seed and zero. got {pixel:?}"
        );
    }
}
