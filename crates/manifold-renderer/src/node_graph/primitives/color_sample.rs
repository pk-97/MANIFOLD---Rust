//! `node.color_sample` — Texture→Scalar bridge. Reads a single pixel
//! from the input texture at a configurable normalised UV and emits
//! two scalar wires: `out` (the RGB triple as a `Scalar(Vec3)`) and
//! `luma` (Rec.709-weighted brightness as `Scalar(F32)`).
//!
//! Simplest possible bridge — no reduction, no atomics, just one
//! `textureLoad`. Use the Vec3 `out` for palette / tint sampling and
//! the `luma` scalar for region-brightness modulation (e.g. a Color
//! Compass picks four cardinal `luma` readings and steers a downstream
//! transformation toward the brightest patch).
//!
//! One frame of latency on the readback, same as the other bridges.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UvUniform {
    uv: [f32; 2],
    /// Half-extent of the region averaged around `uv`, in *pixels*.
    /// `radius=0` reads a single pixel (the historical behaviour);
    /// larger radii average a (2r+1)² window so high-frequency
    /// content doesn't pin the reading on whatever lone pixel happens
    /// to be at the UV. Color Compass sets this to ~16 so its
    /// cardinal samples reflect *region* brightness, not single-pixel
    /// noise.
    radius_px: f32,
    _pad1: f32,
}

crate::primitive! {
    name: ColorSample,
    type_id: "node.color_sample",
    purpose: "Read a single pixel from the input texture at the configured `uv`. Emits `out` (Vec3 RGB) and `luma` (Rec.709-weighted brightness scalar). Bridge for pulling representative colours or per-region brightness out of an image. One frame of latency.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: ScalarVec3,
        luma: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("uv"),
            label: "UV",
            ty: ParamType::Vec2,
            default: ParamValue::Vec2([0.5, 0.5]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("radius_px"),
            label: "Region Radius (px)",
            ty: ParamType::Int,
            // 0 = single-pixel read (backward compatible). Larger
            // radii average a (2r+1)² window — drop high-frequency
            // noise so a "region brightness" wire reflects the eye's
            // perception, not whatever lone texel lives at the UV.
            default: ParamValue::Float(0.0),
            range: Some((0.0, 64.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Single-pixel read at the configured UV (clamped to [0, 1]). Pair upstream with a `MipChain` to sample a *region* average instead of a single texel — sampling mip N reads the box-filtered 2^N×2^N neighbourhood. Use the `luma` port directly when you only need brightness — it's the same Rec.709 weighting Luminance applies frame-wide.",
    examples: [],
    picker: { label: "Color Sample", category: Driver },
    summary: "Reads the colour at a single point in the image and outputs its RGB and brightness. An eyedropper you can drive an effect from.",
    category: DetectionAndSampling,
    role: Control,
    aliases: ["color sample", "eyedropper", "pick color", "probe"],
    boundary_reason: IoBridge,
    extra_fields: {
        measure_buffer: Option<GpuBuffer> = None,
        previous_value: [f32; 3] = [0.0, 0.0, 0.0],
        previous_luma: f32 = 0.0,
    },
}

impl Primitive for ColorSample {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if let Some(ref buf) = self.measure_buffer
            && let Some(ptr) = buf.mapped_ptr()
        {
            // Four contiguous f32s: R, G, B, luma. Shader computes
            // luma once on the GPU side so the CPU emits both wires
            // off a single readback rather than re-deriving brightness
            // from the RGB triple in Rust.
            let p = ptr as *const f32;
            let r = unsafe { std::ptr::read(p) };
            let g = unsafe { std::ptr::read(p.add(1)) };
            let b = unsafe { std::ptr::read(p.add(2)) };
            let luma = unsafe { std::ptr::read(p.add(3)) };
            if [r, g, b, luma].iter().all(|c| c.is_finite()) {
                self.previous_value = [r, g, b];
                self.previous_luma = luma;
            }
        }

        ctx.outputs
            .set_scalar("out", ParamValue::Vec3(self.previous_value));
        ctx.outputs
            .set_scalar("luma", ParamValue::Float(self.previous_luma));

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let uv = match ctx.params.get("uv") {
            Some(ParamValue::Vec2(v)) => *v,
            _ => [0.5, 0.5],
        };
        let radius_px = match ctx.params.get("radius_px") {
            Some(ParamValue::Float(v)) => v.clamp(0.0, 64.0).round(),
            _ => 0.0,
        };

        let gpu = ctx.gpu_encoder();
        // 16 bytes — 3 floats for RGB plus one for alignment padding,
        // matches the shader's `array<f32>` indexing into a 16-byte
        // aligned storage region.
        let measure_buffer = self
            .measure_buffer
            .get_or_insert_with(|| gpu.device.create_buffer_shared(16));
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/color_sample.wgsl"),
                "cs_main",
                "node.color_sample",
            )
        });

        let uniforms = UvUniform {
            uv,
            radius_px,
            _pad1: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: in_tex,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: measure_buffer,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "node.color_sample",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU smoke. No Vec3 consumer exists in the catalog today —
    //! FluidSim2D declares a Vec3 input port but its implementation is
    //! a stub. Tests verify the readback round-trip via a Capture
    //! sink for now; once a real Vec3 consumer ships, this will
    //! convert to a production-shaped test that verifies rendered
    //! pixels.

    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::ColorSample;
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
        seen: std::sync::Arc<std::sync::Mutex<Option<[f32; 3]>>>,
    }
    impl EffectNode for Capture {
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            static INPUTS: [NodeInput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("in"),
                ty: PortType::Scalar(ScalarType::Vec3),
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
            if let Some(ParamValue::Vec3(v)) = ctx.inputs.scalar("in") {
                *self.seen.lock().unwrap() = Some(v);
            }
        }
    }

    struct CaptureFloat {
        type_id: EffectNodeType,
        seen: std::sync::Arc<std::sync::Mutex<Option<f32>>>,
    }
    impl EffectNode for CaptureFloat {
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

    /// A solid colour input should round-trip exactly through the
    /// sampling shader → buffer → scalar wire path. Tests that the
    /// Vec3 wire actually carries Vec3 values end-to-end through the
    /// runtime.
    #[test]
    fn solid_color_round_trips_through_scalar_wire() {
        let device = crate::test_device();
        let (w, h) = (16u32, 16u32);
        let format = GpuTextureFormat::Rgba16Float;
        let color = [0.7_f32, 0.3, 0.2];

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let sample = g.add_node(Box::new(ColorSample::new()));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = g.add_node(Box::new(Capture {
            type_id: EffectNodeType::new("test.capture"),
            seen: seen.clone(),
        }));
        g.connect((src, "out"), (sample, "in")).unwrap();
        g.connect((sample, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let src_target = RenderTarget::new(&device, w, h, format, "test-cs-src");
        crate::clear_texture_committed(
            &device,
            &src_target.texture,
            [color[0] as f64, color[1] as f64, color[2] as f64, 1.0],
            "color-sample-clear",
        );

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_src, src_target);
        let mut exec = Executor::new(Box::new(backend));

        for _ in 0..2 {
            let mut enc = device.create_encoder("color-sample-frame");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
            }
            enc.commit_and_wait_completed();
        }

        let v = seen
            .lock()
            .unwrap()
            .expect("Capture should have seen a Vec3 by frame 2");
        // fp16 storage on the source texture plus one round-trip
        // through f32 storage on the readback buffer keeps tolerance
        // tight.
        for c in 0..3 {
            assert!(
                (v[c] - color[c]).abs() < 0.005,
                "channel {c}: expected {}, got {}",
                color[c],
                v[c],
            );
        }
    }

    /// `luma` port emits Rec.709-weighted brightness of the sampled
    /// pixel. Same weights as the frame-wide `Luminance` primitive,
    /// so a region brightness reading and a global one share a
    /// single definition of "brightness".
    #[test]
    fn luma_port_emits_rec709_brightness() {
        let device = crate::test_device();
        let (w, h) = (16u32, 16u32);
        let format = GpuTextureFormat::Rgba16Float;
        // Rec.709 luma of a deeply saturated green is dominated by the
        // green weight (0.7152). Pick a colour whose channels don't
        // collide so a mis-applied weight (e.g. R vs B swap) shows up
        // in the assertion.
        let color = [0.2_f32, 0.8, 0.4];
        let expected_luma = 0.2126 * color[0] + 0.7152 * color[1] + 0.0722 * color[2];

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let sample = g.add_node(Box::new(ColorSample::new()));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = g.add_node(Box::new(CaptureFloat {
            type_id: EffectNodeType::new("test.capture_float"),
            seen: seen.clone(),
        }));
        g.connect((src, "out"), (sample, "in")).unwrap();
        g.connect((sample, "luma"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let src_target = RenderTarget::new(&device, w, h, format, "test-cs-luma-src");
        crate::clear_texture_committed(
            &device,
            &src_target.texture,
            [color[0] as f64, color[1] as f64, color[2] as f64, 1.0],
            "color-sample-luma-clear",
        );

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_src, src_target);
        let mut exec = Executor::new(Box::new(backend));

        for _ in 0..2 {
            let mut enc = device.create_encoder("color-sample-luma-frame");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
            }
            enc.commit_and_wait_completed();
        }

        let v = seen.lock().unwrap().expect("luma readback by frame 2");
        assert!(
            (v - expected_luma).abs() < 0.005,
            "expected Rec.709 luma {expected_luma}, got {v}",
        );
    }
}
