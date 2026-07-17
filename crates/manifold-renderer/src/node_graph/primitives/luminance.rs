//! `node.luminance` — Texture→Scalar bridge. Reads the average
//! Rec. 709 luminance of the input texture and emits it on a scalar
//! output port.
//!
//! First member of the **bridge** primitive family: nodes that flow
//! data from the image domain into the control domain, completing the
//! responsive-instrument loop (control → image was already shipped via
//! scalar wires shadowing params; this is image → control).
//!
//! ## Latency
//!
//! GPU readback runs at **one frame of latency** to avoid pipeline
//! stalls. Each frame the primitive:
//!   1. Reads the previous frame's result from its shared-mode
//!      `MTLBuffer` (Metal's shared storage mode is CPU+GPU coherent,
//!      so by the time we read it, the previous frame's GPU work has
//!      completed — frame-pacing guarantees the submission ordering).
//!   2. Emits that value on the `out` scalar wire.
//!   3. Dispatches *this* frame's reduction shader, writing into the
//!      same buffer for next frame's read.
//!
//! Initial frame outputs `0.0` (the buffer's default) until enough
//! frames have flowed. One-frame latency is musically invisible on
//! stage (16ms at 60fps) and matches the convention TouchDesigner
//! uses for Analyze TOPs → CHOPs.
//!
//! ## Reduction approach
//!
//! Single workgroup of 16×16 threads sparse-samples the input at 256
//! grid positions, parallel-reduces in workgroup-shared memory,
//! writes a single f32 to the storage buffer. Sparse sampling is
//! "good enough" for control-rate signals; users wanting pixel-exact
//! reduction can chain a `MipChain` upstream first.

use manifold_gpu::{GpuBinding, GpuBuffer};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: Luminance,
    type_id: "node.luminance",
    purpose: "Average Rec. 709 luminance of the input texture, emitted as a scalar on `out` (range [0, 1]). Bridge from image domain to control domain — wire this into any scalar input to make the effect respond to its own brightness. One frame of latency on the measurement.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: ScalarF32,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Texture→Scalar bridge. Sparse-samples at 256 grid positions — fast and constant-time regardless of input resolution, but not pixel-exact. Chain a `MipChain` upstream if exact reduction matters. Output lags input by one frame due to GPU readback.",
    examples: [],
    picker: { label: "Luminance", category: Driver },
    summary: "Measures the average brightness of the image and outputs it as a single number. Wire it into a knob to make an effect react to how bright the picture is.",
    category: DetectionAndSampling,
    role: Control,
    aliases: ["luminance", "brightness", "average", "level"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        measure_buffer: Option<GpuBuffer> = None,
        previous_value: f32 = 0.0,
    },
}

impl Primitive for Luminance {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Read previous frame's measurement (if any) before dispatching
        // this frame's reduction. Shared-mode buffer is CPU-readable
        // without a sync — by the time we run this frame, the previous
        // frame's command buffer has completed, so the value is fresh.
        if let Some(ref buf) = self.measure_buffer
            && let Some(ptr) = buf.mapped_ptr()
        {
            let val = unsafe { std::ptr::read(ptr as *const f32) };
            if val.is_finite() && val >= 0.0 {
                self.previous_value = val.clamp(0.0, 1.0);
            }
        }

        // Emit the (one-frame-stale) measurement on the wire so
        // downstream consumers see a usable value.
        ctx.outputs
            .set_scalar("out", ParamValue::Float(self.previous_value));

        // No input wired — skip dispatch but keep emitting the last
        // captured value. Common during graph editing.
        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };

        let gpu = ctx.gpu_encoder();
        let measure_buffer = self
            .measure_buffer
            .get_or_insert_with(|| gpu.device.create_buffer_shared(16));
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/luminance.wgsl"),
                "cs_main",
                "node.luminance",
            )
        });

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture: in_tex,
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: measure_buffer,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "node.luminance",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU smoke + correctness tests. A solid-color input texture
    //! should reduce to its own luminance; the value lands on the
    //! output scalar wire one frame after the dispatch (so we run two
    //! frames and read the second).

    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::Luminance;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::effect_node::{
        EffectNode, EffectNodeContext, EffectNodeType, FrameTime, NodeInstanceId,
    };
    use crate::node_graph::execution_plan::{ExecutionPlan, ResourceId, compile};
    use crate::node_graph::graph::Graph;
    use crate::node_graph::parameters::{ParamDef, ParamValue};
    use crate::node_graph::ports::{
        NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType,
    };
    use crate::node_graph::{Executor, MetalBackend, Source};
    use crate::render_target::RenderTarget;

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

    fn frame_time(beats: f32) -> FrameTime {
        FrameTime {
            beats: Beats(beats as f64),
            seconds: Seconds(beats as f64 * 0.5),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    /// Scalar sink that records the last value seen on its `in` port.
    struct Capture {
        type_id: EffectNodeType,
        seen: std::sync::Arc<std::sync::Mutex<Option<f32>>>,
    }
    impl EffectNode for Capture {
        fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule { crate::node_graph::depth_rule::DepthRule::Terminal } // test fixture
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            static INPUTS: [NodeInput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("in"),
                ty: PortType::Scalar(ScalarType::F32),
                kind: PortKind::Input,
                required: true,
            }];
            &INPUTS
        }
        fn outputs(&self) -> &[NodeOutput] {
            &[]
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            if let Some(ParamValue::Float(v)) = ctx.inputs.scalar("in") {
                *self.seen.lock().unwrap() = Some(v);
            }
        }
    }

    /// Solid-grey input → grey's luminance lands on the wire after a
    /// one-frame warmup. Tests the full bridge plumbing: shader runs,
    /// shared-buffer readback fires, scalar makes it through the wire
    /// to the downstream consumer.
    #[test]
    fn solid_grey_texture_reduces_to_its_luminance() {
        let device = crate::test_device();
        let (w, h) = (16u32, 16u32);
        let format = GpuTextureFormat::Rgba16Float;
        // Grey at 0.4 → Rec.709 luma = 0.4*(0.2126+0.7152+0.0722) = 0.4 exactly.
        let grey = 0.4_f32;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let lum = g.add_node(Box::new(Luminance::new()));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = g.add_node(Box::new(Capture {
            type_id: EffectNodeType::new("test.capture"),
            seen: seen.clone(),
        }));
        // No FinalOutput — when absent, the validator runs every node
        // and its required-input check, which is exactly what we want
        // for this scalar-only test path.
        g.connect((src, "out"), (lum, "in")).unwrap();
        g.connect((lum, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let src_target = RenderTarget::new(&device, w, h, format, "test-lum-src");
        let mut native_enc = device.create_encoder("luminance-test");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            gpu.clear_texture(
                &src_target.texture,
                grey as f64,
                grey as f64,
                grey as f64,
                1.0,
            );
        }

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_src, src_target);
        let mut exec = Executor::new(Box::new(backend));

        // Frame 1: dispatch reduction, no measurement yet (default 0).
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(0.0), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        // Frame 2: read frame-1's result, emit on wire. Sink captures it.
        let mut native_enc2 = device.create_encoder("luminance-test-2");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc2, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(0.0), &mut gpu);
        }
        native_enc2.commit_and_wait_completed();

        let observed = seen.lock().unwrap();
        let v = observed.expect("Capture should have seen a scalar by frame 2");
        // Tolerance loose enough for fp16 storage in the source texture
        // (clear_texture writes Rgba16Float) plus the sparse-sample
        // averaging — every grid sample reads ~0.4 so the average is
        // pinned tight.
        assert!(
            (v - grey).abs() < 0.005,
            "expected luminance ~={grey}, got {v}",
        );
    }

    /// Black texture should reduce to 0.0; white to ~1.0. Sanity check
    /// the two boundaries to make sure the bridge isn't silently
    /// returning a constant.
    #[test]
    fn black_and_white_textures_reduce_to_their_luminance() {
        for (name, rgb, expected) in [("black", [0.0; 3], 0.0_f32), ("white", [1.0; 3], 1.0_f32)] {
            let device = crate::test_device();
            let (w, h) = (16u32, 16u32);
            let format = GpuTextureFormat::Rgba16Float;

            let mut g = Graph::new();
            let src = g.add_node(Box::new(Source::new()));
            let lum = g.add_node(Box::new(Luminance::new()));
            let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
            let sink = g.add_node(Box::new(Capture {
                type_id: EffectNodeType::new("test.capture"),
                seen: seen.clone(),
            }));
            g.connect((src, "out"), (lum, "in")).unwrap();
            g.connect((lum, "out"), (sink, "in")).unwrap();
            let plan = compile(&g).unwrap();

            let r_src = output_resource(&plan, src, "out");
            let src_target =
                RenderTarget::new(&device, w, h, format, "test-lum-bw-src");
            crate::clear_texture_committed(
                &device,
                &src_target.texture,
                [rgb[0], rgb[1], rgb[2], 1.0],
                "luminance-bw-clear",
            );

            let mut backend =
                MetalBackend::new(device.arc(), w, h, format);
            backend.pre_bind_texture_2d(r_src, src_target);
            let mut exec = Executor::new(Box::new(backend));

            for _ in 0..2 {
                let mut enc = device.create_encoder("luminance-bw-frame");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    exec.execute_frame_with_gpu(&mut g, &plan, frame_time(0.0), &mut gpu);
                }
                enc.commit_and_wait_completed();
            }

            let observed = seen.lock().unwrap();
            let v = observed.expect("Capture should have seen a scalar");
            assert!(
                (v - expected).abs() < 0.01,
                "{name} input expected luminance ~={expected}, got {v}",
            );
        }
    }
}
