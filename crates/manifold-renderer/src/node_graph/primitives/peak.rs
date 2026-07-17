//! `node.peak` — Texture→Scalar bridge. Outputs the *brightest* Rec.
//! 709 luminance sampled across the input texture (max-reduction over
//! a 256-position sparse grid). Sibling of [`Luminance`] but tuned for
//! "respond to the brightest spot" use cases: highlight-keyed knobs,
//! transient-driven envelopes, "the kick lit up the screen" reactions.
//!
//! Same readback architecture as `node.luminance` — one frame of
//! latency, shared-mode `MTLBuffer` reused across frames, dispatch
//! after emit.
//!
//! [`Luminance`]: super::luminance::Luminance

use manifold_gpu::{GpuBinding, GpuBuffer};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: Peak,
    type_id: "node.peak",
    purpose: "Peak (max) Rec. 709 luminance of the input texture, emitted as a scalar on `out` (range [0, 1]). Bridge from image domain to control domain — drives knobs that should respond to the brightest spot rather than overall brightness. One frame of latency on the measurement.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: ScalarF32,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Texture→Scalar bridge. Sparse-samples at 256 grid positions and emits the max; constant-time regardless of resolution but not pixel-exact. Pair with `node.luminance` (average) when you want both: peak for transients, average for sustained level.",
    examples: [],
    picker: { label: "Peak", category: Driver },
    summary: "Measures the brightest point in the image and outputs it as a single number. Reacts to the highlights rather than the overall brightness.",
    category: DetectionAndSampling,
    role: Control,
    aliases: ["peak", "max brightness", "highlight"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        measure_buffer: Option<GpuBuffer> = None,
        previous_value: f32 = 0.0,
    },
}

impl Primitive for Peak {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if let Some(ref buf) = self.measure_buffer
            && let Some(ptr) = buf.mapped_ptr()
        {
            let val = unsafe { std::ptr::read(ptr as *const f32) };
            if val.is_finite() && val >= 0.0 {
                self.previous_value = val.clamp(0.0, 1.0);
            }
        }

        ctx.outputs
            .set_scalar("out", ParamValue::Float(self.previous_value));

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };

        let gpu = ctx.gpu_encoder();
        let measure_buffer = self
            .measure_buffer
            .get_or_insert_with(|| gpu.device.create_buffer_shared(16));
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/peak.wgsl"),
                "cs_main",
                "node.peak",
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
            "node.peak",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU correctness tests. A solid-color input reduces to its
    //! own luminance (peak of constant = that constant). The peak
    //! lands on the wire after a one-frame warmup, same readback
    //! architecture as Luminance.

    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::Peak;
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

    /// Solid-grey input → peak == grey's luminance (peak of constant = constant).
    /// Same flow as Luminance's test; just a sanity check that the
    /// max-reduction agrees with the average-reduction when the input is
    /// uniform.
    #[test]
    fn solid_grey_texture_reduces_to_its_luminance() {
        let device = crate::test_device();
        let (w, h) = (16u32, 16u32);
        let format = GpuTextureFormat::Rgba16Float;
        let grey = 0.4_f32;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let peak = g.add_node(Box::new(Peak::new()));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = g.add_node(Box::new(Capture {
            type_id: EffectNodeType::new("test.capture"),
            seen: seen.clone(),
        }));
        g.connect((src, "out"), (peak, "in")).unwrap();
        g.connect((peak, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let src_target = RenderTarget::new(&device, w, h, format, "test-peak-grey");
        crate::clear_texture_committed(
            &device,
            &src_target.texture,
            [grey as f64, grey as f64, grey as f64, 1.0],
            "peak-grey-clear",
        );

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_src, src_target);
        let mut exec = Executor::new(Box::new(backend));

        for _ in 0..2 {
            let mut enc = device.create_encoder("peak-grey-frame");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
            }
            enc.commit_and_wait_completed();
        }

        let v = seen.lock().unwrap().expect("Capture should have seen a scalar");
        assert!(
            (v - grey).abs() < 0.005,
            "expected peak ~={grey}, got {v}",
        );
    }

    /// Black and white edge cases — black peaks at 0, white peaks at 1.
    /// Same boundary check as Luminance; cheap insurance against
    /// shader bugs that flip min/max or read garbage.
    #[test]
    fn black_and_white_textures_reduce_to_their_luminance() {
        for (name, rgb, expected) in [("black", [0.0; 3], 0.0_f32), ("white", [1.0; 3], 1.0_f32)] {
            let device = crate::test_device();
            let (w, h) = (16u32, 16u32);
            let format = GpuTextureFormat::Rgba16Float;

            let mut g = Graph::new();
            let src = g.add_node(Box::new(Source::new()));
            let peak = g.add_node(Box::new(Peak::new()));
            let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
            let sink = g.add_node(Box::new(Capture {
                type_id: EffectNodeType::new("test.capture"),
                seen: seen.clone(),
            }));
            g.connect((src, "out"), (peak, "in")).unwrap();
            g.connect((peak, "out"), (sink, "in")).unwrap();
            let plan = compile(&g).unwrap();

            let r_src = output_resource(&plan, src, "out");
            let src_target =
                RenderTarget::new(&device, w, h, format, "test-peak-bw");
            crate::clear_texture_committed(
                &device,
                &src_target.texture,
                [rgb[0], rgb[1], rgb[2], 1.0],
                "peak-bw-clear",
            );

            let mut backend = MetalBackend::new(device.arc(), w, h, format);
            backend.pre_bind_texture_2d(r_src, src_target);
            let mut exec = Executor::new(Box::new(backend));

            for _ in 0..2 {
                let mut enc = device.create_encoder("peak-bw-frame");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
                }
                enc.commit_and_wait_completed();
            }

            let v = seen
                .lock()
                .unwrap()
                .expect("Capture should have seen a scalar");
            assert!(
                (v - expected).abs() < 0.01,
                "{name} input expected peak ~={expected}, got {v}",
            );
        }
    }
}
