//! Color-domain primitives: [`Brightness`], [`ChannelMix`], [`ColorRamp`].
//!
//! All three are pixel-local: each output pixel depends only on the same
//! input pixel and parameters. They will fuse cleanly with each other and
//! with other pixel-local primitives once the fusion compiler lands.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};

const SOURCE_INPUT: NodeInput = NodePort {
    name: "source",
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
};

const OUT_OUTPUT: NodeOutput = NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
};

// =====================================================================
// Brightness — RGB → grayscale via per-channel weights.
// =====================================================================

pub const BRIGHTNESS_TYPE_ID: &str = "node.brightness";

const BRIGHTNESS_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const BRIGHTNESS_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const BRIGHTNESS_PARAMS: [ParamDef; 1] = [ParamDef {
    name: "weights",
    label: "RGB Weights",
    ty: ParamType::Vec3,
    // Rec. 709 luma coefficients.
    default: ParamValue::Vec3([0.2126, 0.7152, 0.0722]),
    range: None,
    enum_values: &[],
}];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BrightnessUniforms {
    weights: [f32; 4], // xyz used; w padding
}

pub struct Brightness {
    type_id: EffectNodeType,
    pipeline: Option<manifold_gpu::GpuComputePipeline>,
    sampler: Option<manifold_gpu::GpuSampler>,
}

impl Brightness {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(BRIGHTNESS_TYPE_ID),
            pipeline: None,
            sampler: None,
        }
    }
}

impl Default for Brightness {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Brightness {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &BRIGHTNESS_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &BRIGHTNESS_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &BRIGHTNESS_PARAMS
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let w = match ctx.params.get("weights") {
            Some(ParamValue::Vec3(v)) => *v,
            _ => [0.2126, 0.7152, 0.0722],
        };
        let uniforms = BrightnessUniforms {
            weights: [w[0], w[1], w[2], 0.0],
        };

        let Some(in_tex) = ctx.inputs.texture_2d("source") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/brightness.wgsl"),
                "cs_main",
                "node.brightness",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

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
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.brightness",
        );
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: BRIGHTNESS_TYPE_ID,
        create: || Box::new(Brightness::new()),
        picker: Some(crate::node_graph::palette::PickerInfo { label: "Brightness", category: crate::node_graph::palette::PaletteCategory::Atom }),
    }
}

// =====================================================================
// ChannelMix — 4x4 RGBA transformation.
// =====================================================================

pub const CHANNEL_MIX_TYPE_ID: &str = "node.channel_mixer";

const CHANNEL_MIX_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const CHANNEL_MIX_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const CHANNEL_MIX_PARAMS: [ParamDef; 4] = [
    ParamDef {
        name: "row0",
        label: "Row 0 (R)",
        ty: ParamType::Vec4,
        default: ParamValue::Vec4([1.0, 0.0, 0.0, 0.0]),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "row1",
        label: "Row 1 (G)",
        ty: ParamType::Vec4,
        default: ParamValue::Vec4([0.0, 1.0, 0.0, 0.0]),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "row2",
        label: "Row 2 (B)",
        ty: ParamType::Vec4,
        default: ParamValue::Vec4([0.0, 0.0, 1.0, 0.0]),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "row3",
        label: "Row 3 (A)",
        ty: ParamType::Vec4,
        default: ParamValue::Vec4([0.0, 0.0, 0.0, 1.0]),
        range: None,
        enum_values: &[],
    },
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChannelMixUniforms {
    row0: [f32; 4],
    row1: [f32; 4],
    row2: [f32; 4],
    row3: [f32; 4],
}

pub struct ChannelMix {
    type_id: EffectNodeType,
    pipeline: Option<manifold_gpu::GpuComputePipeline>,
    sampler: Option<manifold_gpu::GpuSampler>,
}

impl ChannelMix {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(CHANNEL_MIX_TYPE_ID),
            pipeline: None,
            sampler: None,
        }
    }
}

impl Default for ChannelMix {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for ChannelMix {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &CHANNEL_MIX_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &CHANNEL_MIX_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &CHANNEL_MIX_PARAMS
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let row = |name: &str, default: [f32; 4]| -> [f32; 4] {
            match ctx.params.get(name) {
                Some(ParamValue::Vec4(v)) => *v,
                _ => default,
            }
        };
        let uniforms = ChannelMixUniforms {
            row0: row("row0", [1.0, 0.0, 0.0, 0.0]),
            row1: row("row1", [0.0, 1.0, 0.0, 0.0]),
            row2: row("row2", [0.0, 0.0, 1.0, 0.0]),
            row3: row("row3", [0.0, 0.0, 0.0, 1.0]),
        };

        let Some(in_tex) = ctx.inputs.texture_2d("source") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/channel_mix.wgsl"),
                "cs_main",
                "node.channel_mixer",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

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
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.channel_mixer",
        );
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: CHANNEL_MIX_TYPE_ID,
        create: || Box::new(ChannelMix::new()),
        picker: Some(crate::node_graph::palette::PickerInfo { label: "Channel Mixer", category: crate::node_graph::palette::PaletteCategory::Atom }),
    }
}

// =====================================================================
// ColorRamp — luma → two-stop gradient lookup.
// =====================================================================

pub const COLOR_RAMP_TYPE_ID: &str = "node.gradient_map";

const COLOR_RAMP_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const COLOR_RAMP_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const COLOR_RAMP_PARAMS: [ParamDef; 2] = [
    ParamDef {
        name: "color_a",
        label: "Color A",
        ty: ParamType::Color,
        default: ParamValue::Color([0.0, 0.0, 0.0, 1.0]),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: "color_b",
        label: "Color B",
        ty: ParamType::Color,
        default: ParamValue::Color([1.0, 1.0, 1.0, 1.0]),
        range: None,
        enum_values: &[],
    },
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ColorRampUniforms {
    color_a: [f32; 4],
    color_b: [f32; 4],
}

pub struct ColorRamp {
    type_id: EffectNodeType,
    pipeline: Option<manifold_gpu::GpuComputePipeline>,
    sampler: Option<manifold_gpu::GpuSampler>,
}

impl ColorRamp {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(COLOR_RAMP_TYPE_ID),
            pipeline: None,
            sampler: None,
        }
    }
}

impl Default for ColorRamp {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for ColorRamp {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &COLOR_RAMP_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &COLOR_RAMP_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &COLOR_RAMP_PARAMS
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let color = |name: &str, default: [f32; 4]| -> [f32; 4] {
            match ctx.params.get(name) {
                Some(ParamValue::Color(c)) => *c,
                Some(ParamValue::Vec4(v)) => *v,
                _ => default,
            }
        };
        let uniforms = ColorRampUniforms {
            color_a: color("color_a", [0.0, 0.0, 0.0, 1.0]),
            color_b: color("color_b", [1.0, 1.0, 1.0, 1.0]),
        };

        let Some(in_tex) = ctx.inputs.texture_2d("source") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/color_ramp.wgsl"),
                "cs_main",
                "node.gradient_map",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

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
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.gradient_map",
        );
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: COLOR_RAMP_TYPE_ID,
        create: || Box::new(ColorRamp::new()),
        picker: Some(crate::node_graph::palette::PickerInfo { label: "Gradient Map", category: crate::node_graph::palette::PaletteCategory::Atom }),
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod channel_mix_gpu_tests {
    //! Hardware tests for the channel_mix 4x4 matrix transform.
    //! Verify the canonical use cases: identity (default), A→R swizzle
    //! (the StarField use case), and channel isolation.
    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::ChannelMix;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::execution_plan::{ExecutionPlan, ResourceId, compile};
    use crate::node_graph::graph::Graph;
    use crate::node_graph::parameters::ParamValue;
    use crate::node_graph::{
        Executor, FinalOutput, FrameTime, MetalBackend, NodeInstanceId, Source,
    };
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

    /// Render a single ChannelMix node with the given matrix rows over
    /// an input texture cleared to `src_rgba`. Return the first pixel's
    /// RGBA as f32.
    fn run_channel_mix(
        src_rgba: [f32; 4],
        row0: [f32; 4],
        row1: [f32; 4],
        row2: [f32; 4],
        row3: [f32; 4],
    ) -> [f32; 4] {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let node = g.add_node(Box::new(ChannelMix::new()));
        let sink = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(node, "row0", ParamValue::Vec4(row0)).unwrap();
        g.set_param(node, "row1", ParamValue::Vec4(row1)).unwrap();
        g.set_param(node, "row2", ParamValue::Vec4(row2)).unwrap();
        g.set_param(node, "row3", ParamValue::Vec4(row3)).unwrap();
        g.connect((src, "out"), (node, "source")).unwrap();
        g.connect((node, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let r_out = output_resource(&plan, node, "out");
        let src_target = RenderTarget::new(&device, w, h, format, "channel-mix-src");
        let out_target = RenderTarget::new(&device, w, h, format, "channel-mix-out");
        crate::clear_texture_committed(
            &device,
            &src_target.texture,
            [
                src_rgba[0] as f64,
                src_rgba[1] as f64,
                src_rgba[2] as f64,
                src_rgba[3] as f64,
            ],
            "channel-mix-src-clear",
        );

        let mut backend = MetalBackend::new(&device, w, h, format);
        backend.pre_bind_texture_2d(r_src, src_target);
        let out_slot = backend.pre_bind_texture_2d(r_out, out_target);

        let mut native_enc = device.create_encoder("channel-mix-frame");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let tex = exec.backend().texture_2d(out_slot).expect("retained");
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut readback_enc = device.create_encoder("channel-mix-readback");
        readback_enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback.mapped_ptr().expect("shared");
        let pixels: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        [
            f16::from_bits(pixels[0]).to_f32(),
            f16::from_bits(pixels[1]).to_f32(),
            f16::from_bits(pixels[2]).to_f32(),
            f16::from_bits(pixels[3]).to_f32(),
        ]
    }

    /// Default matrix = identity. Output should match input.
    #[test]
    fn identity_matrix_preserves_input() {
        let src = [0.4_f32, 0.6, 0.2, 0.8];
        let out = run_channel_mix(
            src,
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        );
        for i in 0..4 {
            assert!(
                (out[i] - src[i]).abs() < 0.02,
                "identity matrix changed channel {i}: out={} src={}",
                out[i],
                src[i]
            );
        }
    }

    /// Swap A → R. With src.a = 0.8, expect R = 0.8 in output.
    /// (The StarField use case: voronoi cell_hash → R for downstream
    /// per-pixel math.)
    #[test]
    fn swap_a_to_r_moves_alpha_to_red() {
        let src = [0.4_f32, 0.6, 0.2, 0.8];
        let out = run_channel_mix(
            src,
            [0.0, 0.0, 0.0, 1.0], // R = src.a
            [0.0, 0.0, 0.0, 0.0], // G = 0
            [0.0, 0.0, 0.0, 0.0], // B = 0
            [0.0, 0.0, 0.0, 1.0], // A = src.a (passthrough)
        );
        assert!((out[0] - src[3]).abs() < 0.02, "R should equal src.a: out.r={}, src.a={}", out[0], src[3]);
        assert!(out[1].abs() < 0.02, "G should be zero: {}", out[1]);
        assert!(out[2].abs() < 0.02, "B should be zero: {}", out[2]);
        assert!((out[3] - src[3]).abs() < 0.02, "A should pass through: out.a={}, src.a={}", out[3], src[3]);
    }

    /// Luma drop: each output channel = Rec.709 luma of input RGB.
    #[test]
    fn luma_matrix_grayscales() {
        let src = [1.0_f32, 0.0, 0.0, 1.0]; // pure red
        let luma_row = [0.2126, 0.7152, 0.0722, 0.0];
        let out = run_channel_mix(
            src,
            luma_row,
            luma_row,
            luma_row,
            [0.0, 0.0, 0.0, 1.0],
        );
        let expected = 0.2126_f32;
        for (i, &val) in out.iter().enumerate().take(3) {
            assert!(
                (val - expected).abs() < 0.02,
                "luma channel {i}: out={val} expected={expected}",
            );
        }
        assert!((out[3] - 1.0).abs() < 0.02, "alpha passthrough: {}", out[3]);
    }
}
