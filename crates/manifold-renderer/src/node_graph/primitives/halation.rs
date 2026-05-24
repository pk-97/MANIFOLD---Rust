//! `node.halation` — pixel-exact replacement for legacy
//! Originally `HalationFX`. Fused
//! composite.
//!
//! Three passes: Pass 0 fuses threshold, tint, and H Gaussian
//! per-tap (no fp16 intermediate); Pass 1 is V Gaussian; Pass 2 is
//! composite (source plus halo times amount). Splitting Pass 0 into
//! atomic threshold-tint then GaussianBlur H would round through
//! fp16 at the intermediate texture and lose bit-exact parity. Same
//! reason Glitch, Strobe, EdgeDetect, VoronoiPrism, and Bloom were
//! fused.
//!
//! Two intermediate textures owned by the primitive (`buf_b` for the
//! H-blur output, `buf_a` for the V-blur output). Rebuilt on size
//! change; `clear_state()` drops them.

use std::sync::OnceLock;

use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuSampler, GpuSamplerDesc, GpuTextureFormat};

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::node_graph::primitive::PrimitiveDescription;
use crate::render_target::RenderTarget;

const HALATION_WGSL: &str = include_str!("shaders/halation.wgsl");

pub const HALATION_TYPE_ID: &str = "node.halation";

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HalationUniforms {
    mode: u32,
    amount: f32,
    threshold: f32,
    spread: f32,
    tint_r: f32,
    tint_g: f32,
    tint_b: f32,
    main_texel_size_x: f32,
    main_texel_size_y: f32,
    halo_texel_size_x: f32,
    halo_texel_size_y: f32,
    _pad: f32,
}

/// Fused-composite Halation primitive. Owns two ping-pong textures.
pub struct Halation {
    pipeline_h_blur: Option<GpuComputePipeline>,
    pipeline_v_blur: Option<GpuComputePipeline>,
    pipeline_composite: Option<GpuComputePipeline>,
    sampler: Option<GpuSampler>,
    buf_a: Option<RenderTarget>,
    buf_b: Option<RenderTarget>,
    buf_dims: Option<(u32, u32)>,
}

impl Halation {
    pub fn new() -> Self {
        Self {
            pipeline_h_blur: None,
            pipeline_v_blur: None,
            pipeline_composite: None,
            sampler: None,
            buf_a: None,
            buf_b: None,
            buf_dims: None,
        }
    }

    fn ensure_buffers(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.buf_dims == Some((width, height)) {
            return;
        }
        let format = GpuTextureFormat::Rgba16Float;
        self.buf_a = Some(RenderTarget::new(
            device,
            width,
            height,
            format,
            "HalationA",
        ));
        self.buf_b = Some(RenderTarget::new(
            device,
            width,
            height,
            format,
            "HalationB",
        ));
        self.buf_dims = Some((width, height));
    }

    fn ensure_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.pipeline_h_blur.is_none() {
            self.pipeline_h_blur = Some(device.create_specialized_compute_pipeline(
                HALATION_WGSL,
                "cs_main",
                &[("uniforms.mode", "0u")],
                "node.halation.h_blur",
            ));
        }
        if self.pipeline_v_blur.is_none() {
            self.pipeline_v_blur = Some(device.create_specialized_compute_pipeline(
                HALATION_WGSL,
                "cs_main",
                &[("uniforms.mode", "1u")],
                "node.halation.v_blur",
            ));
        }
        if self.pipeline_composite.is_none() {
            self.pipeline_composite = Some(device.create_specialized_compute_pipeline(
                HALATION_WGSL,
                "cs_main",
                &[("uniforms.mode", "2u")],
                "node.halation.composite",
            ));
        }
        if self.sampler.is_none() {
            self.sampler = Some(device.create_sampler(&GpuSamplerDesc::default()));
        }
    }
}

impl Default for Halation {
    fn default() -> Self {
        Self::new()
    }
}

const HALATION_INPUTS: [NodeInput; 1] = [NodePort {
    name: "in",
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
}];

const HALATION_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const HALATION_PARAMS: [ParamDef; 5] = [
    ParamDef {
        name: "amount",
        label: "Amount",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "threshold",
        label: "Threshold",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "spread",
        label: "Spread",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "hue",
        label: "Hue",
        ty: ParamType::Float,
        default: ParamValue::Float(20.0),
        range: Some((0.0, 360.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "saturation",
        label: "Saturation",
        ty: ParamType::Float,
        default: ParamValue::Float(0.6),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
];

fn cached_type_id() -> &'static EffectNodeType {
    static CELL: OnceLock<EffectNodeType> = OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(HALATION_TYPE_ID))
}

/// HSV-to-RGB matching legacy `HalationFX::hsv_to_rgb` bit-for-bit.
/// Operates on the host side so the shader only sees pre-converted
/// tint floats (matches legacy ordering: param decode → HSV → uniform pack).
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let h = (h / 360.0).rem_euclid(1.0);
    if s <= 0.0 {
        return (v, v, v);
    }
    let hh = h * 6.0;
    let sector = hh as i32;
    let frac = hh - sector as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * frac);
    let t = v * (1.0 - s * (1.0 - frac));
    match sector % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

impl Halation {
    /// AI-composition surface metadata.
    pub fn description() -> PrimitiveDescription {
        PrimitiveDescription {
            type_id: HALATION_TYPE_ID,
            purpose: "Filmic halo: extracts bright pixels above `threshold`, tints them by an HSV-defined color, blurs with a 17-tap separable Gaussian, and composites the halo back over the source by `amount`.",
            composition_notes: "Fused composite — Pass 0 applies threshold-tint per Gaussian tap, which doesn't decompose into atomic threshold + GaussianBlur without fp16 intermediate quantization.",
            examples: &["preset.effect.halation"],
            inputs: &HALATION_INPUTS,
            outputs: &HALATION_OUTPUTS,
            params: &HALATION_PARAMS,
        }
    }
}

impl EffectNode for Halation {
    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }
    fn inputs(&self) -> &[NodeInput] {
        &HALATION_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &HALATION_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &HALATION_PARAMS
    }
    fn clear_state(&mut self) {
        self.buf_a = None;
        self.buf_b = None;
        self.buf_dims = None;
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = read_f32(ctx, "amount", 0.0);
        let threshold = read_f32(ctx, "threshold", 0.5);
        let spread = read_f32(ctx, "spread", 0.5);
        let hue = read_f32(ctx, "hue", 20.0);
        let saturation = read_f32(ctx, "saturation", 0.6);
        let (tint_r, tint_g, tint_b) = hsv_to_rgb(hue, saturation, 1.0);

        let Some(source) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (target.width, target.height);

        let gpu = ctx.gpu_encoder();
        self.ensure_pipelines(gpu.device);
        self.ensure_buffers(gpu.device, width, height);

        let p_h = self.pipeline_h_blur.as_ref().unwrap();
        let p_v = self.pipeline_v_blur.as_ref().unwrap();
        let p_c = self.pipeline_composite.as_ref().unwrap();
        let sampler = self.sampler.as_ref().unwrap();
        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        let base = HalationUniforms {
            mode: 0,
            amount,
            threshold,
            spread,
            tint_r,
            tint_g,
            tint_b,
            main_texel_size_x: 0.0,
            main_texel_size_y: 0.0,
            halo_texel_size_x: 0.0,
            halo_texel_size_y: 0.0,
            _pad: 0.0,
        };

        // Pass 0: ThresholdTint + H Gaussian → buf_b
        let pass0_u = HalationUniforms {
            mode: 0,
            main_texel_size_x: 1.0 / width as f32,
            main_texel_size_y: 1.0 / height as f32,
            ..base
        };
        dispatch_halation(
            gpu,
            p_h,
            source,
            source,
            &buf_b.texture,
            sampler,
            &pass0_u,
            width,
            height,
            "node.halation.h_blur",
        );

        // Pass 1: V Gaussian → buf_a
        let pass1_u = HalationUniforms {
            mode: 1,
            main_texel_size_x: 1.0 / width as f32,
            main_texel_size_y: 1.0 / height as f32,
            ..base
        };
        dispatch_halation(
            gpu,
            p_v,
            &buf_b.texture,
            &buf_b.texture,
            &buf_a.texture,
            sampler,
            &pass1_u,
            width,
            height,
            "node.halation.v_blur",
        );

        // Pass 2: Composite source + buf_a × amount → target
        let pass2_u = HalationUniforms {
            mode: 2,
            main_texel_size_x: 1.0 / width as f32,
            main_texel_size_y: 1.0 / height as f32,
            halo_texel_size_x: 1.0 / width as f32,
            halo_texel_size_y: 1.0 / height as f32,
            ..base
        };
        dispatch_halation(
            gpu,
            p_c,
            source,
            &buf_a.texture,
            target,
            sampler,
            &pass2_u,
            width,
            height,
            "node.halation.composite",
        );
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: HALATION_TYPE_ID,
        create: || Box::new(Halation::new()),
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
fn dispatch_halation(
    gpu: &mut crate::gpu_encoder::GpuEncoder<'_>,
    pipeline: &GpuComputePipeline,
    source_a: &manifold_gpu::GpuTexture,
    source_b: &manifold_gpu::GpuTexture,
    target: &manifold_gpu::GpuTexture,
    sampler: &GpuSampler,
    uniforms: &HalationUniforms,
    width: u32,
    height: u32,
    label: &str,
) {
    gpu.native_enc.dispatch_compute(
        pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(uniforms),
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
