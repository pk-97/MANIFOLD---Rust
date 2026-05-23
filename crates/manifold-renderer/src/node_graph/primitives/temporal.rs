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
    },
    outputs: {
        out: Texture2D,
    },
    params: [],
    composition_notes: "Wire the loop's final output back into `in`, and read `out` upstream as the previous frame. State is per-`(NodeInstanceId, OwnerKey)` so multiple layers / clips using the same chain get independent feedback streams. First-frame semantics: `out` mirrors `in` (no uninitialised pixels).",
    examples: ["preset.effect.stylized_feedback", "preset.effect.mandala", "preset.effect.smear_mosh"],
    picker: { label: "Feedback", category: Atom },
}

const FEEDBACK_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

/// Per-`(NodeInstanceId, OwnerKey)` persistent state — the previous
/// frame's input. Held by the runtime's `StateStore`.
struct FeedbackState {
    prev: RenderTarget,
    width: u32,
    height: u32,
}

impl NodeState for FeedbackState {}

impl Primitive for Feedback {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);
        if width == 0 || height == 0 {
            return;
        }

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;

        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("Feedback::run requires a GpuEncoder");
        let store = ctx
            .state
            .as_deref_mut()
            .expect("Feedback::run requires a StateStore");

        // Lazy-init the persistent prev-frame buffer. Seeded from `in`
        // on first allocation so first-frame `out` is a pass-through
        // rather than uninit garbage — matches `node.array_feedback`'s
        // first-frame contract. Resized (re-allocated) if dims change.
        let needs_alloc = match store.get::<FeedbackState>(node_id, owner_key) {
            Some(s) => s.width != width || s.height != height,
            None => true,
        };
        if needs_alloc {
            let prev = if let Some(pool) = gpu.pool {
                RenderTarget::new_pooled(pool, width, height, FEEDBACK_FORMAT, "feedback prev")
            } else {
                RenderTarget::new(gpu.device, width, height, FEEDBACK_FORMAT, "feedback prev")
            };
            // Seed prev from `in` so first-frame `out` reads the
            // current input (pass-through). Subsequent frames see the
            // *actual* previous input.
            gpu.copy_texture_to_texture(in_tex, &prev.texture, width, height);
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
        // Capture this frame's `in` for next frame's `out`.
        gpu.copy_texture_to_texture(in_tex, &state.prev.texture, width, height);
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

    fn breaks_dependency_cycle(&self) -> bool {
        // `in` is a state capture for next frame, not a per-frame
        // dependency. Lets feedback chains like
        // `source → ... → mix → feedback → ... → mix` close their
        // loop in the wire graph instead of inside the primitive.
        true
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
}
