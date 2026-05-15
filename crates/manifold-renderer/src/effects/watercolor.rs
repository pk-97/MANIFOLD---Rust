// Watercolor — multi-pass feedback effect simulating watercolor paint flow.
// Ported from TouchDesigner watercolor tutorial signal chain.
//
// 7-pass pipeline per frame:
//   Pass 1 (Grain+Max):      source + grain + max(feedback * decay) → temp_a
//   Pass 2 (Flow Map):       fBM noise → flow_map (half-res, every frame)
//   Pass 3 (Displacement):   displace temp_a by flow_map → temp_b
//   Pass 4 (Blur):           Gaussian blur temp_b → temp_a
//   Pass 5 (Slope Displace): soft light → Sobel → displace temp_a → temp_b
//   Pass 6 (Luma Blur):      17-tap variable blur with noise mask → feedback
//   Pass 7 (Blend):          wet/dry mix of feedback with source → target
//
// Performance:
//   - 4-octave fBM (octaves 5–10 contribute <3% combined)
//   - Flow map at half resolution (25% dispatch cost, bilinear upsample)
//   - Grain inlined into max composite (eliminates 1 dispatch + 1 texture)
//   - Luma blur reduced to 17 taps (2 rings) for lower texture bandwidth

use std::borrow::Cow;

use super::compute_dual_blit_helper::ComputeDualBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::Watercolor;
use crate::node_graph::{
    ChainSpec, Graph, NodeInstanceId, ParamConvert, Routing, SkipMode, SpliceResult,
};
use crate::render_target::RenderTarget;
use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::new("Watercolor"),
        display_name: "Watercolor",
        category: "Post-Process",
        available: true,
        osc_prefix: "watercolor",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.5, "F2", ""),
            ParamSpec::continuous("displace", "Displace", 0.0001, 0.01, 0.001, "F4", "displace"),
            ParamSpec::continuous("blur", "Blur", 0.5, 8.0, 2.0, "F1", "blur"),
            ParamSpec::continuous("decay", "Decay", 0.9, 1.0, 0.99, "F3", "decay"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::new("Watercolor"),
        create: |device| Box::new(WatercolorFX::new(device)),
    }
}

fn splice_watercolor(graph: &mut Graph, source: (NodeInstanceId, &'static str)) -> SpliceResult {
    let node = graph.add_node(Box::new(Watercolor::new()));
    graph.connect(source, (node, "in")).expect("wire source → Watercolor.in");
    SpliceResult {
        output: (node, "out"),
        handles: vec![(Cow::Borrowed("watercolor"), node)],
    }
}

inventory::submit! {
    ChainSpec {
        type_id: EffectTypeId::new("Watercolor"),
        splice: splice_watercolor,
        routings: &[
            Routing { param_id: "amount", target_handle: "watercolor", target_param: "amount", convert: ParamConvert::Float },
            Routing { param_id: "displace", target_handle: "watercolor", target_param: "displace", convert: ParamConvert::Float },
            Routing { param_id: "blur", target_handle: "watercolor", target_param: "blur", convert: ParamConvert::Float },
            Routing { param_id: "decay", target_handle: "watercolor", target_param: "decay", convert: ParamConvert::Float },
            // `time` is ctx-driven, populated by `apply_ctx_params_at`.
        ],
        skip: SkipMode::OnZero { param_id: "amount" },
    }
}

const WATERCOLOR_WGSL: &str = include_str!("shaders/fx_watercolor_compute.wgsl");

// Uniforms — 64 bytes, 16-byte aligned. Field order matches WGSL.
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

/// Per-owner state: persistent feedback buffer + frame-lifetime intermediates.
struct WatercolorState {
    feedback: RenderTarget, // Persistent across frames (Rgba16Float)
    flow_map: RenderTarget, // Flow displacement map (RB channels)
    temp_a: RenderTarget,   // Intermediate A (full-res)
    temp_b: RenderTarget,   // Intermediate B (full-res)
}

pub struct WatercolorFX {
    helper: ComputeDualBlitHelper,
    pipeline_max: manifold_gpu::GpuComputePipeline, // mode 1 (grain + max)
    pipeline_flow_gen: manifold_gpu::GpuComputePipeline, // mode 2
    pipeline_displace: manifold_gpu::GpuComputePipeline, // mode 3
    pipeline_blur: manifold_gpu::GpuComputePipeline, // mode 4
    pipeline_slope: manifold_gpu::GpuComputePipeline, // mode 5
    pipeline_luma: manifold_gpu::GpuComputePipeline, // mode 6
    pipeline_blend: manifold_gpu::GpuComputePipeline, // mode 7 (wet/dry)
    states: AHashMap<i64, WatercolorState>,
    width: u32,
    height: u32,
}

fn alloc_target(
    device: &manifold_gpu::GpuDevice,
    pool: Option<&manifold_gpu::TexturePool>,
    w: u32,
    h: u32,
    label: &str,
) -> RenderTarget {
    let fmt = manifold_gpu::GpuTextureFormat::Rgba16Float;
    if let Some(p) = pool {
        RenderTarget::new_pooled(p, w, h, fmt, label)
    } else {
        RenderTarget::new(device, w, h, fmt, label)
    }
}

impl WatercolorFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let spec = |mode: &str, label: &str| {
            device.create_specialized_compute_pipeline(
                WATERCOLOR_WGSL,
                "cs_main",
                &[("uniforms.mode", mode)],
                label,
            )
        };
        Self {
            helper: ComputeDualBlitHelper::new(device, WATERCOLOR_WGSL, "Watercolor Compute"),
            pipeline_max: spec("1u", "WC GrainMax"),
            pipeline_flow_gen: spec("2u", "WC FlowGen"),
            pipeline_displace: spec("3u", "WC Displace"),
            pipeline_blur: spec("4u", "WC Blur"),
            pipeline_slope: spec("5u", "WC Slope"),
            pipeline_luma: spec("6u", "WC Luma"),
            pipeline_blend: spec("7u", "WC Blend"),
            states: AHashMap::new(),
            width: 0,
            height: 0,
        }
    }
}

impl PostProcessEffect for WatercolorFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::WATERCOLOR
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        self.width = ctx.width;
        self.height = ctx.height;

        // Ensure per-owner state exists — clear feedback to black on creation
        if !self.states.contains_key(&ctx.owner_key) && self.width > 0 && self.height > 0 {
            let w = self.width;
            let h = self.height;
            let feedback = alloc_target(gpu.device, gpu.pool, w, h, "WC Feedback");
            gpu.clear_texture(&feedback.texture, 0.0, 0.0, 0.0, 0.0);
            self.states.insert(
                ctx.owner_key,
                WatercolorState {
                    feedback,
                    flow_map: alloc_target(
                        gpu.device,
                        gpu.pool,
                        (w / 2).max(1),
                        (h / 2).max(1),
                        "WC FlowMap",
                    ),
                    temp_a: alloc_target(gpu.device, gpu.pool, w, h, "WC TempA"),
                    temp_b: alloc_target(gpu.device, gpu.pool, w, h, "WC TempB"),
                },
            );
        }

        let w = ctx.width;
        let h = ctx.height;
        let state = self.states.get(&ctx.owner_key).unwrap();

        // Extract user-facing parameters
        let amount = fx.param_values.first().map(|p| p.value).unwrap_or(0.5);
        let displace_weight = fx
            .param_values
            .get(1)
            .map(|p| p.value)
            .unwrap_or(0.001)
            .clamp(0.0001, 0.01);
        let blur_radius = fx
            .param_values
            .get(2)
            .map(|p| p.value)
            .unwrap_or(2.0)
            .clamp(0.5, 8.0);
        let decay = fx
            .param_values
            .get(3)
            .map(|p| p.value)
            .unwrap_or(0.99)
            .clamp(0.9, 1.0);

        let uniforms = WatercolorUniforms {
            mode: 0, // overridden by function constants per pipeline
            time: ctx.time,
            width: w as f32,
            height: h as f32,
            displace_weight,
            blur_radius,
            emboss_strength: 0.0, // unused — field kept for uniform alignment
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

        // Pass 1: Grain + Max Composite — source ⊕ (feedback * decay) → temp_a
        self.helper.dispatch_with(
            &self.pipeline_max,
            gpu,
            source,
            &state.feedback.texture,
            &state.temp_a.texture,
            ubytes,
            "WC GrainMax",
            w,
            h,
        );

        // Pass 2: Flow Map — procedural noise → flow_map (half-res)
        // Half resolution cuts dispatch to 25% cost; bilinear sampling in
        // the displacement pass smoothly upscales. Runs every frame for
        // consistent frame pacing.
        let flow_w = (w / 2).max(1);
        let flow_h = (h / 2).max(1);
        self.helper.dispatch_a_only_with(
            &self.pipeline_flow_gen,
            gpu,
            source, // not read, just bound for layout
            &state.flow_map.texture,
            ubytes,
            "WC FlowGen",
            flow_w,
            flow_h,
        );

        // Pass 3: Displacement — temp_a + flow_map → temp_b
        self.helper.dispatch_with(
            &self.pipeline_displace,
            gpu,
            &state.temp_a.texture,
            &state.flow_map.texture,
            &state.temp_b.texture,
            ubytes,
            "WC Displace",
            w,
            h,
        );

        // Pass 4: Edge Diffusion Blur — temp_b → temp_a
        self.helper.dispatch_a_only_with(
            &self.pipeline_blur,
            gpu,
            &state.temp_b.texture,
            &state.temp_a.texture,
            ubytes,
            "WC Blur",
            w,
            h,
        );

        // Pass 5: Slope Displacement — source + temp_a → temp_b
        self.helper.dispatch_with(
            &self.pipeline_slope,
            gpu,
            source,
            &state.temp_a.texture,
            &state.temp_b.texture,
            ubytes,
            "WC Slope",
            w,
            h,
        );

        // Pass 6: Luma Blur — temp_b → feedback (persistent buffer)
        self.helper.dispatch_a_only_with(
            &self.pipeline_luma,
            gpu,
            &state.temp_b.texture,
            &state.feedback.texture,
            ubytes,
            "WC Luma",
            w,
            h,
        );

        // Pass 7: Wet/Dry Blend — feedback + source → target (final output)
        self.helper.dispatch_with(
            &self.pipeline_blend,
            gpu,
            &state.feedback.texture,
            source,
            target,
            ubytes,
            "WC Blend",
            w,
            h,
        );
    }

    fn clear_state(&mut self) {
        self.states.clear();
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.states.clear();
    }

}
