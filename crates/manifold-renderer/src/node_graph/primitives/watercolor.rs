//! `node.watercolor` — pixel-exact replacement for legacy
//! Originally `WatercolorFX`. Fused
//! composite.
//!
//! Seven sequential passes: grain+max → flow generation → displacement
//! → diffusion blur → slope displacement → luma blur (writes
//! persistent feedback) → wet/dry composite. Watercolor's diffusion
//! pass uses a 2D non-separable 9-tap kernel and the slope pass
//! reads neighbors from both source and prior-pass buffers, so the
//! decomposition into atomic primitives would round through fp16
//! intermediates at multiple boundaries and lose bit-exact parity.
//! Ships as a fused composite like Bloom, Halation, and the four
//! §6.1 fused composites; the future fusion compiler can expose
//! the underlying atoms when there's a real use case.
//!
//! State: persistent `feedback` texture (carries to next frame),
//! frame-lifetime `temp_a` + `temp_b` ping-pong, half-res
//! `flow_map`. Rebuilt on size change; `clear_state()` clears
//! feedback to black (seek-correct).
//!
//! Until the legacy effect is deleted in §6.6 cutover, the shader
//! is shared with `effects/shaders/fx_watercolor_compute.wgsl` via
//! `include_str!` — keeps the parity test honest (no risk of the
//! primitive and legacy drifting on a shader edit).

use std::borrow::Cow;
use std::sync::OnceLock;

use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuSampler, GpuSamplerDesc, GpuTextureFormat};

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use crate::node_graph::primitive::PrimitiveDescription;
use crate::render_target::RenderTarget;

const WATERCOLOR_WGSL: &str = include_str!("../../effects/shaders/fx_watercolor_compute.wgsl");

pub const WATERCOLOR_TYPE_ID: &str = "node.watercolor";

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WatercolorUniforms {
    mode: u32,
    time: f32,
    width: f32,
    height: f32,
    displace_weight: f32,
    blur_radius: f32,
    emboss_strength: f32,
    amount: f32,
    slope_strength: f32,
    slope_step: f32,
    luma_blur_radius: f32,
    grain_amount: f32,
    decay: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

/// Fused-composite Watercolor primitive.
pub struct Watercolor {
    pipeline_max: Option<GpuComputePipeline>,
    pipeline_flow_gen: Option<GpuComputePipeline>,
    pipeline_displace: Option<GpuComputePipeline>,
    pipeline_blur: Option<GpuComputePipeline>,
    pipeline_slope: Option<GpuComputePipeline>,
    pipeline_luma: Option<GpuComputePipeline>,
    pipeline_blend: Option<GpuComputePipeline>,
    sampler: Option<GpuSampler>,
    feedback: Option<RenderTarget>,
    flow_map: Option<RenderTarget>,
    temp_a: Option<RenderTarget>,
    temp_b: Option<RenderTarget>,
    state_dims: Option<(u32, u32)>,
    feedback_needs_clear: bool,
}

impl Watercolor {
    pub fn new() -> Self {
        Self {
            pipeline_max: None,
            pipeline_flow_gen: None,
            pipeline_displace: None,
            pipeline_blur: None,
            pipeline_slope: None,
            pipeline_luma: None,
            pipeline_blend: None,
            sampler: None,
            feedback: None,
            flow_map: None,
            temp_a: None,
            temp_b: None,
            state_dims: None,
            feedback_needs_clear: false,
        }
    }

    fn ensure_state(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.state_dims == Some((width, height)) {
            return;
        }
        let format = GpuTextureFormat::Rgba16Float;
        let flow_w = (width / 2).max(1);
        let flow_h = (height / 2).max(1);
        self.feedback = Some(RenderTarget::new(
            device,
            width,
            height,
            format,
            "WC Feedback",
        ));
        self.flow_map = Some(RenderTarget::new(
            device,
            flow_w,
            flow_h,
            format,
            "WC FlowMap",
        ));
        self.temp_a = Some(RenderTarget::new(device, width, height, format, "WC TempA"));
        self.temp_b = Some(RenderTarget::new(device, width, height, format, "WC TempB"));
        self.state_dims = Some((width, height));
        self.feedback_needs_clear = true;
    }

    fn ensure_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.pipeline_max.is_none() {
            self.pipeline_max = Some(device.create_specialized_compute_pipeline(
                WATERCOLOR_WGSL,
                "cs_main",
                &[("uniforms.mode", "1u")],
                "node.watercolor.max",
            ));
        }
        if self.pipeline_flow_gen.is_none() {
            self.pipeline_flow_gen = Some(device.create_specialized_compute_pipeline(
                WATERCOLOR_WGSL,
                "cs_main",
                &[("uniforms.mode", "2u")],
                "node.watercolor.flow_gen",
            ));
        }
        if self.pipeline_displace.is_none() {
            self.pipeline_displace = Some(device.create_specialized_compute_pipeline(
                WATERCOLOR_WGSL,
                "cs_main",
                &[("uniforms.mode", "3u")],
                "node.watercolor.displace",
            ));
        }
        if self.pipeline_blur.is_none() {
            self.pipeline_blur = Some(device.create_specialized_compute_pipeline(
                WATERCOLOR_WGSL,
                "cs_main",
                &[("uniforms.mode", "4u")],
                "node.watercolor.blur",
            ));
        }
        if self.pipeline_slope.is_none() {
            self.pipeline_slope = Some(device.create_specialized_compute_pipeline(
                WATERCOLOR_WGSL,
                "cs_main",
                &[("uniforms.mode", "5u")],
                "node.watercolor.slope",
            ));
        }
        if self.pipeline_luma.is_none() {
            self.pipeline_luma = Some(device.create_specialized_compute_pipeline(
                WATERCOLOR_WGSL,
                "cs_main",
                &[("uniforms.mode", "6u")],
                "node.watercolor.luma",
            ));
        }
        if self.pipeline_blend.is_none() {
            self.pipeline_blend = Some(device.create_specialized_compute_pipeline(
                WATERCOLOR_WGSL,
                "cs_main",
                &[("uniforms.mode", "7u")],
                "node.watercolor.blend",
            ));
        }
        if self.sampler.is_none() {
            self.sampler = Some(device.create_sampler(&GpuSamplerDesc::default()));
        }
    }
}

impl Default for Watercolor {
    fn default() -> Self {
        Self::new()
    }
}

const WATERCOLOR_INPUTS: [NodeInput; 2] = [
    NodePort {
        name: Cow::Borrowed("in"),
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: true,
    },
    // Port-shadows-param for the `time` param. Wire wins, param is the
    // fallback. Lets a preset wire `system.generator_input.time` into
    // this port — replaces the hardcoded `apply_ctx_params_at` time
    // injection on the chain runner.
    NodePort {
        name: Cow::Borrowed("time"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
];

const WATERCOLOR_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const WATERCOLOR_PARAMS: [ParamDef; 5] = [
    ParamDef {
        name: Cow::Borrowed("amount"),
        label: "Amount",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("displace"),
        label: "Displace",
        ty: ParamType::Float,
        default: ParamValue::Float(0.001),
        range: Some((0.0001, 0.01)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("blur"),
        label: "Blur",
        ty: ParamType::Float,
        default: ParamValue::Float(2.0),
        range: Some((0.5, 8.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("decay"),
        label: "Decay",
        ty: ParamType::Float,
        default: ParamValue::Float(0.99),
        range: Some((0.9, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("time"),
        label: "Time",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((0.0, 1e9)),
        enum_values: &[],
    },
];

fn cached_type_id() -> &'static EffectNodeType {
    static CELL: OnceLock<EffectNodeType> = OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(WATERCOLOR_TYPE_ID))
}

impl Watercolor {
    /// AI-composition surface metadata.
    pub fn description() -> PrimitiveDescription {
        PrimitiveDescription {
            type_id: WATERCOLOR_TYPE_ID,
            purpose: "TouchDesigner-style watercolor: 7-pass simulation with feedback. Grain + max composite → fBM flow displacement → diffusion blur → slope displacement → luma blur into feedback → wet/dry composite with source.",
            composition_notes: "Fused composite — feedback loop plus non-separable 9-tap diffusion blur and dual-input slope pass don't decompose into atomic primitives without fp16 intermediates breaking bit-exact parity. Owns persistent feedback state; clear_state() resets feedback to black.",
            examples: &["preset.effect.watercolor"],
            inputs: &WATERCOLOR_INPUTS,
            outputs: &WATERCOLOR_OUTPUTS,
            params: &WATERCOLOR_PARAMS,
        }
    }
}

impl EffectNode for Watercolor {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Warp
    }
    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }
    fn boundary_reason(&self) -> Option<crate::node_graph::freeze::classify::BoundaryReason> {
        Some(crate::node_graph::freeze::classify::BoundaryReason::ConversionDebt)
    }
    fn inputs(&self) -> &[NodeInput] {
        &WATERCOLOR_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &WATERCOLOR_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &WATERCOLOR_PARAMS
    }
    fn clear_state(&mut self) {
        self.feedback = None;
        self.flow_map = None;
        self.temp_a = None;
        self.temp_b = None;
        self.state_dims = None;
        self.feedback_needs_clear = false;
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = read_f32(ctx, "amount", 0.5);
        let displace_weight = read_f32(ctx, "displace", 0.001).clamp(0.0001, 0.01);
        let blur_radius = read_f32(ctx, "blur", 2.0).clamp(0.5, 8.0);
        let decay = read_f32(ctx, "decay", 0.99).clamp(0.9, 1.0);
        // Port-shadows-param: wire wins, param is the fallback. Lets a
        // preset wire `system.generator_input.time` into this port —
        // replaces the hardcoded `apply_ctx_params_at` injection.
        let time = match ctx.inputs.scalar("time") {
            Some(ParamValue::Float(f)) => f,
            _ => read_f32(ctx, "time", 0.0),
        };

        let Some(source) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (target.width, target.height);

        let gpu = ctx.gpu_encoder();
        self.ensure_pipelines(gpu.device);
        self.ensure_state(gpu.device, width, height);

        // Clear feedback on first frame after (re)allocation. Matches
        // legacy `apply` which clears on `ensure_state`.
        if self.feedback_needs_clear {
            gpu.clear_texture(&self.feedback.as_ref().unwrap().texture, 0.0, 0.0, 0.0, 0.0);
            self.feedback_needs_clear = false;
        }

        let p_max = self.pipeline_max.as_ref().unwrap();
        let p_flow = self.pipeline_flow_gen.as_ref().unwrap();
        let p_disp = self.pipeline_displace.as_ref().unwrap();
        let p_blur = self.pipeline_blur.as_ref().unwrap();
        let p_slope = self.pipeline_slope.as_ref().unwrap();
        let p_luma = self.pipeline_luma.as_ref().unwrap();
        let p_blend = self.pipeline_blend.as_ref().unwrap();
        let sampler = self.sampler.as_ref().unwrap();
        let feedback = self.feedback.as_ref().unwrap();
        let flow_map = self.flow_map.as_ref().unwrap();
        let temp_a = self.temp_a.as_ref().unwrap();
        let temp_b = self.temp_b.as_ref().unwrap();

        let uniforms = WatercolorUniforms {
            mode: 0,
            time,
            width: width as f32,
            height: height as f32,
            displace_weight,
            blur_radius,
            emboss_strength: 0.0,
            amount,
            slope_strength: 5.0,
            slope_step: 5.0,
            luma_blur_radius: 10.0,
            grain_amount: 0.15,
            decay,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        let ubytes = bytemuck::bytes_of(&uniforms);

        // Pass 1: Grain + Max — source ⊕ (feedback * decay) → temp_a
        dispatch_watercolor(
            gpu,
            p_max,
            source,
            &feedback.texture,
            &temp_a.texture,
            sampler,
            ubytes,
            width,
            height,
            "node.watercolor.max",
        );

        // Pass 2: Flow Map (half-res)
        let flow_w = flow_map.width;
        let flow_h = flow_map.height;
        dispatch_watercolor(
            gpu,
            p_flow,
            source,
            source,
            &flow_map.texture,
            sampler,
            ubytes,
            flow_w,
            flow_h,
            "node.watercolor.flow_gen",
        );

        // Pass 3: Displacement — temp_a + flow_map → temp_b
        dispatch_watercolor(
            gpu,
            p_disp,
            &temp_a.texture,
            &flow_map.texture,
            &temp_b.texture,
            sampler,
            ubytes,
            width,
            height,
            "node.watercolor.displace",
        );

        // Pass 4: Edge Diffusion Blur — temp_b → temp_a
        dispatch_watercolor(
            gpu,
            p_blur,
            &temp_b.texture,
            &temp_b.texture,
            &temp_a.texture,
            sampler,
            ubytes,
            width,
            height,
            "node.watercolor.blur",
        );

        // Pass 5: Slope Displacement — source + temp_a → temp_b
        dispatch_watercolor(
            gpu,
            p_slope,
            source,
            &temp_a.texture,
            &temp_b.texture,
            sampler,
            ubytes,
            width,
            height,
            "node.watercolor.slope",
        );

        // Pass 6: Luma Blur — temp_b → feedback (persistent)
        dispatch_watercolor(
            gpu,
            p_luma,
            &temp_b.texture,
            &temp_b.texture,
            &feedback.texture,
            sampler,
            ubytes,
            width,
            height,
            "node.watercolor.luma",
        );

        // Pass 7: Wet/Dry Blend — feedback + source → target
        dispatch_watercolor(
            gpu,
            p_blend,
            &feedback.texture,
            source,
            target,
            sampler,
            ubytes,
            width,
            height,
            "node.watercolor.blend",
        );
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: WATERCOLOR_TYPE_ID,
        create: || Box::new(Watercolor::new()),
        picker: None,
    }
}

fn read_f32(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
    match ctx.params.get(name) {
        Some(ParamValue::Float(f)) => *f,
        _ => default,
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_watercolor(
    gpu: &mut crate::gpu_encoder::GpuEncoder<'_>,
    pipeline: &GpuComputePipeline,
    source_a: &manifold_gpu::GpuTexture,
    source_b: &manifold_gpu::GpuTexture,
    target: &manifold_gpu::GpuTexture,
    sampler: &GpuSampler,
    uniform_bytes: &[u8],
    width: u32,
    height: u32,
    label: &str,
) {
    gpu.native_enc.dispatch_compute(
        pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: uniform_bytes,
            },
            GpuBinding::Texture {
                binding: 1,
                texture: source_a,
            },
            GpuBinding::Texture {
                binding: 2,
                texture: source_b,
            },
            GpuBinding::Sampler {
                binding: 3,
                sampler,
            },
            GpuBinding::Texture {
                binding: 4,
                texture: target,
            },
        ],
        [width.div_ceil(16), height.div_ceil(16), 1],
        label,
    );
}
