//! Composition primitives — combine two textures into one.
//!
//! [`Mix`] is a linear crossfade. [`Blend`] applies one of several blend
//! modes with an opacity. Both are pixel-local and fuseable.

use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuSampler, GpuSamplerDesc};

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};

const OUT_OUTPUT: NodeOutput = NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
};

// =====================================================================
// Mix — linear crossfade A → B.
// =====================================================================

pub const MIX_TYPE_ID: &str = "primitive.mix";

const MIX_INPUTS: [NodeInput; 2] = [
    NodePort {
        name: "a",
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: true,
    },
    NodePort {
        name: "b",
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: true,
    },
];

const MIX_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const MIX_PARAMS: [ParamDef; 1] = [ParamDef {
    name: "amount",
    label: "Amount",
    ty: ParamType::Float,
    default: ParamValue::Float(0.5),
    range: Some((0.0, 1.0)),
    enum_values: &[],
}];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MixUniforms {
    amount: f32,
    _pad: [f32; 3],
}

/// Linear crossfade `mix(a, b, amount)`. First production-grade
/// `EffectNode` — both the canonical template for other two-input
/// primitives and the smoke test for the runtime's GPU integration
/// (real `MetalBackend` slots, real `GpuEncoder` dispatch, real WGSL
/// pipeline compile).
pub struct Mix {
    type_id: EffectNodeType,
    pipeline: Option<GpuComputePipeline>,
    sampler: Option<GpuSampler>,
}

impl Mix {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(MIX_TYPE_ID),
            pipeline: None,
            sampler: None,
        }
    }
}

impl Default for Mix {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Mix {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &MIX_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &MIX_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &MIX_PARAMS
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = match ctx.params.get("amount") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        };

        // Resolve textures up-front. NodeInputs/NodeOutputs::texture_2d
        // returns refs tied to the backend's lifetime, so they survive
        // the encoder's mutable borrow below.
        let Some(a) = ctx.inputs.texture_2d("a") else {
            return;
        };
        let Some(b) = ctx.inputs.texture_2d("b") else {
            return;
        };
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out.width, out.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/mix.wgsl"),
                "cs_main",
                "primitive.mix",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = MixUniforms {
            amount,
            _pad: [0.0; 3],
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
                    texture: a,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: b,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: out,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "primitive.mix",
        );
    }
}

// =====================================================================
// Blend — composite two textures with a blend mode.
// =====================================================================

pub const BLEND_TYPE_ID: &str = "primitive.blend";

pub const BLEND_MODES: &[&str] = &[
    "Normal",
    "Add",
    "Multiply",
    "Screen",
    "Overlay",
    "Difference",
];

const BLEND_INPUTS: [NodeInput; 2] = [
    NodePort {
        name: "base",
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: true,
    },
    NodePort {
        name: "overlay",
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: true,
    },
];

const BLEND_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const BLEND_PARAMS: [ParamDef; 2] = [
    ParamDef {
        name: "mode",
        label: "Blend Mode",
        ty: ParamType::Enum,
        default: ParamValue::Enum(0), // Normal
        range: None,
        enum_values: BLEND_MODES,
    },
    ParamDef {
        name: "opacity",
        label: "Opacity",
        ty: ParamType::Float,
        default: ParamValue::Float(1.0),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
];

#[derive(Debug)]
pub struct Blend {
    type_id: EffectNodeType,
}

impl Blend {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(BLEND_TYPE_ID),
        }
    }
}

impl Default for Blend {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Blend {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &BLEND_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &BLEND_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &BLEND_PARAMS
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}

#[cfg(test)]
mod gpu_tests {
    //! Real-GPU integration tests. These spin up a `manifold_gpu::GpuDevice`,
    //! a `MetalBackend`, and an actual `GpuEncoder`, then run the graph
    //! end-to-end. Mac-only (Metal).
    //!
    //! Goal: catch wiring bugs (binding indices, format mismatches,
    //! pipeline compilation failures, missing usages) and prove pixel
    //! correctness — bugs that mock-backend tests can't see.

    use std::sync::Arc;

    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::{GpuDevice, GpuTextureFormat};

    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::Slot;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::{
        compile, primitives::compose::Mix, ExecutionPlan, Executor, FinalOutput, FrameTime, Graph,
        MetalBackend, NodeInstanceId, ParamValue, Source,
    };
    use crate::render_target::RenderTarget;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
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

    /// End-to-end smoke test: build `Source × 2 → Mix → FinalOutput`,
    /// dispatch through `Executor::execute_frame_with_gpu`, commit, wait.
    /// Verifies the whole stack — pipeline compile, binding layout,
    /// MetalBackend slot allocation, encoder dispatch — works on a real
    /// `GpuDevice`. No pixel check; that's `mix_pixel_correct_at_half`.
    #[test]
    fn mix_dispatches_through_metal_backend() {
        let device = Arc::new(GpuDevice::new());
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        // Build graph.
        let mut g = Graph::new();
        let src_a = g.add_node(Box::new(Source::new()));
        let src_b = g.add_node(Box::new(Source::new()));
        let mix = g.add_node(Box::new(Mix::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(mix, "amount", ParamValue::Float(0.5)).unwrap();
        g.connect((src_a, "out"), (mix, "a")).unwrap();
        g.connect((src_b, "out"), (mix, "b")).unwrap();
        g.connect((mix, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        // Wire backend + encoder.
        let backend = MetalBackend::new(device.clone(), w, h, format);
        let mut native_enc = device.create_encoder("mix-smoke");
        let mut exec = Executor::new(Box::new(backend));

        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }

        // Synchronously commit + wait so any Metal validation error or
        // shader compile failure surfaces inside the test instead of
        // dangling on the GPU.
        native_enc.commit_and_wait_completed();
    }

    /// Pixel-accurate proof of correctness. Pre-binds host-supplied
    /// red and blue input textures to the two `Source` nodes via
    /// `MetalBackend::pre_bind_texture_2d`, runs Mix at amount=0.5, and
    /// reads back Mix's output. Expected per-pixel: (0.5, 0.0, 0.5, 1.0)
    /// (within f16 precision tolerance). This is the proof that
    /// shader bindings, slot allocation, idempotent acquire, and the
    /// dispatch math are all wired correctly.
    #[test]
    fn mix_pixel_correct_at_half() {
        let device = Arc::new(GpuDevice::new());
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        // Build graph.
        let mut g = Graph::new();
        let src_a = g.add_node(Box::new(Source::new()));
        let src_b = g.add_node(Box::new(Source::new()));
        let mix = g.add_node(Box::new(Mix::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(mix, "amount", ParamValue::Float(0.5)).unwrap();
        g.connect((src_a, "out"), (mix, "a")).unwrap();
        g.connect((src_b, "out"), (mix, "b")).unwrap();
        g.connect((mix, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        // Look up Source A/B's output ResourceIds — what Mix.a and Mix.b
        // will read from after pre-binding.
        let r_a = output_resource(&plan, src_a, "out");
        let r_b = output_resource(&plan, src_b, "out");

        // Allocate the input textures and clear them with known colors.
        let red_target = RenderTarget::new(&device, w, h, format, "test-red");
        let blue_target = RenderTarget::new(&device, w, h, format, "test-blue");
        let mut native_enc = device.create_encoder("mix-pixel");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            gpu.clear_texture(&red_target.texture, 1.0, 0.0, 0.0, 1.0);
            gpu.clear_texture(&blue_target.texture, 0.0, 0.0, 1.0, 1.0);
        }

        // Pre-bind the colored targets to the Source output ResourceIds.
        // Capture the next-slot watermark — Mix's output will be allocated
        // there since the Texture2D free pool is empty post-pre-bind.
        let mut backend = MetalBackend::new(device.clone(), w, h, format);
        backend.pre_bind_texture_2d(r_a, red_target);
        backend.pre_bind_texture_2d(r_b, blue_target);
        let mix_output_slot = Slot(backend.slot_count());

        // Execute the dispatch in the same encoder as the input clears.
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        // Blit Mix's output texture into a CPU-mapped buffer for readback.
        let mix_tex = exec
            .backend()
            .texture_2d(mix_output_slot)
            .expect("mix output texture should be retained on backend");
        let bytes_per_row = w * 8; // Rgba16Float = 8 bytes/pixel.
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("mix-readback");
        readback_enc.copy_texture_to_buffer(mix_tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        // Verify pixel (0,0). Solid colors mean every pixel matches.
        let ptr = readback_buf
            .mapped_ptr()
            .expect("shared buffer should expose mapped pointer");
        let pixels: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        let r = f16::from_bits(pixels[0]).to_f32();
        let g_chan = f16::from_bits(pixels[1]).to_f32();
        let b = f16::from_bits(pixels[2]).to_f32();
        let a = f16::from_bits(pixels[3]).to_f32();
        let tol = 0.01;
        assert!(
            (r - 0.5).abs() < tol,
            "red channel {r} != 0.5 (mix(1,0,0.5))"
        );
        assert!(g_chan.abs() < tol, "green {g_chan} != 0.0");
        assert!(
            (b - 0.5).abs() < tol,
            "blue {b} != 0.5 (mix(0,1,0.5))"
        );
        assert!((a - 1.0).abs() < tol, "alpha {a} != 1.0");
    }
}
