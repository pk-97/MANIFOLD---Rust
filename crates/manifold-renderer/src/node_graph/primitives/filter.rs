//! Filter primitives: [`Threshold`] (pixel-local), [`Blur`] (neighborhood),
//! [`MipChain`] (multi-pass).
//!
//! These three exercise the different fusion categories: Threshold is fully
//! fuseable, Blur breaks fusion with its input but accepts pixel-local
//! tail-fusion, and MipChain runs a series of passes regardless.

use manifold_gpu::{
    GpuBinding, GpuComputePipeline, GpuSampler, GpuSamplerDesc, GpuTexture, GpuTextureFormat,
};
use std::borrow::Cow;

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::render_target::RenderTarget;

const SOURCE_INPUT: NodeInput = NodePort {
    name: Cow::Borrowed("source"),
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
};

const OUT_OUTPUT: NodeOutput = NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
};

// =====================================================================
// Threshold — keep pixels above a luma cutoff (with optional softness).
// =====================================================================

pub const THRESHOLD_TYPE_ID: &str = "node.threshold";

const THRESHOLD_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const THRESHOLD_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const THRESHOLD_PARAMS: [ParamDef; 2] = [
    ParamDef {
        name: Cow::Borrowed("level"),
        label: "Threshold",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("softness"),
        label: "Softness",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ThresholdUniforms {
    level: f32,
    softness: f32,
    _pad0: f32,
    _pad1: f32,
}

pub struct Threshold {
    type_id: EffectNodeType,
    pipeline: Option<GpuComputePipeline>,
    sampler: Option<GpuSampler>,
}

impl Threshold {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(THRESHOLD_TYPE_ID),
            pipeline: None,
            sampler: None,
        }
    }
}

impl Default for Threshold {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for Threshold {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Inherit
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &THRESHOLD_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &THRESHOLD_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &THRESHOLD_PARAMS
    }
    // Hand-written node (no `primitive!` macro), so the fusion contract is
    // declared directly. The body is a verbatim port of threshold.wgsl's
    // response curve; the hand kernel stays authoritative for the standalone
    // dispatch.
    fn fusion_kind(&self) -> crate::node_graph::freeze::classify::FusionKind {
        crate::node_graph::freeze::classify::FusionKind::Pointwise
    }
    fn wgsl_body(&self) -> Option<&'static str> {
        Some(include_str!("shaders/threshold_body.wgsl"))
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let level = ctx.param_f32("level", 0.5);

        let softness = ctx.param_f32("softness", 0.0);

        let Some(source) = ctx.inputs.texture_2d("source") else {
            return;
        };
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out.width, out.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/threshold.wgsl"),
                "cs_main",
                "node.threshold",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ThresholdUniforms {
            level,
            softness,
            _pad0: 0.0,
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
                    texture: source,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.threshold",
        );
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: THRESHOLD_TYPE_ID,
        create: || Box::new(Threshold::new()),
        picker: Some(crate::node_graph::palette::PickerInfo { label: "Threshold", category: crate::node_graph::palette::PaletteCategory::Atom }),
    }
}

// =====================================================================
// Blur — Gaussian/Box/Radial neighborhood blur.
// =====================================================================

pub const BLUR_TYPE_ID: &str = "node.blur";

pub const BLUR_MODES: &[&str] = &["Gaussian", "Box", "Radial", "Smooth"];

const BLUR_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const BLUR_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

const BLUR_PARAMS: [ParamDef; 2] = [
    ParamDef {
        name: Cow::Borrowed("radius"),
        label: "Radius",
        ty: ParamType::Float,
        default: ParamValue::Float(4.0),
        range: Some((0.0, 64.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("mode"),
        label: "Mode",
        ty: ParamType::Enum,
        default: ParamValue::Enum(0), // Gaussian
        range: None,
        enum_values: BLUR_MODES,
    },
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurUniforms {
    radius: f32,
    mode: u32,
    direction: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DualBlurUniforms {
    operation: u32,
    _pad0: u32,
    blend: f32,
    _pad1: f32,
}

const DUAL_BLUR_MODE: u32 = 3;
// A u32-sized image reaches 1x1 within this many reductions. Cache the whole
// geometric pyramid so authored radii beyond the display range remain useful.
const DUAL_BLUR_MAX_LEVEL: usize = u32::BITS as usize;
// Per-axis variance of one area-down/eight-tap-up reconstruction in source
// pixels. The down taps contribute 0.25; the up taps contribute 1.25 from
// their offsets plus 0.1875 from bilinear interpolation in coarse pixels,
// or 5.75 source pixels after the 2× scale. A later level adds another stage
// at the current scale, hence v(d) = 6 + 4*v(d-1). The radius control keeps
// the legacy Gaussian meaning sigma = radius / 2 and interpolates this
// variance ladder.
const DUAL_BLUR_FIRST_VARIANCE: f32 = 6.0;
const DUAL_BLUR_DOWN: u32 = 0;
const DUAL_BLUR_UP: u32 = 1;
const DUAL_BLUR_COPY: u32 = 2;
const DUAL_BLUR_LERP: u32 = 3;

fn dual_radius_level(radius: f32) -> (usize, f32) {
    let radius = if radius.is_finite() {
        radius.max(0.0)
    } else {
        0.0
    };
    let target_variance = (radius * 0.5).powi(2);
    if target_variance <= f32::EPSILON {
        return (0, 0.0);
    }
    for upper_level in 1..=DUAL_BLUR_MAX_LEVEL {
        let lower_variance = dual_level_variance(upper_level - 1);
        let upper_variance = dual_level_variance(upper_level);
        if target_variance < upper_variance || upper_level == DUAL_BLUR_MAX_LEVEL {
            let blend = ((target_variance - lower_variance) / (upper_variance - lower_variance))
                .clamp(0.0, 1.0);
            return (upper_level - 1, blend);
        }
    }
    (DUAL_BLUR_MAX_LEVEL, 0.0)
}

fn dual_level_variance(level: usize) -> f32 {
    let mut variance = 0.0;
    for _ in 0..level {
        variance = DUAL_BLUR_FIRST_VARIANCE + 4.0 * variance;
    }
    variance
}

/// Format for the per-instance scratch texture used as the ping-pong
/// target between Blur's horizontal and vertical passes. Matches the
/// GRAPH_FORMAT used by graph-backed effects.
const BLUR_SCRATCH_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

pub struct Blur {
    type_id: EffectNodeType,
    pipeline: Option<GpuComputePipeline>,
    sampler: Option<GpuSampler>,
    dual_pipeline: Option<GpuComputePipeline>,
    /// Per-instance ping-pong target. Allocated on first dispatch and
    /// reused across frames; recreated when the output dimensions
    /// change (rare — only on resolution change).
    scratch: Option<RenderTarget>,
    dual_pyramid: Option<DualBlurPyramid>,
}

/// Resize-only storage for Smooth mode. `down[level]` is a half-resolution
/// pyramid level (level 1 is the first downsample); the two up arrays hold the
/// adjacent reconstructions needed for continuous radius interpolation.
struct DualBlurPyramid {
    width: u32,
    height: u32,
    down: Vec<RenderTarget>,
    up_a: Vec<RenderTarget>,
    up_b: Vec<RenderTarget>,
}

impl Blur {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(BLUR_TYPE_ID),
            pipeline: None,
            sampler: None,
            dual_pipeline: None,
            scratch: None,
            dual_pyramid: None,
        }
    }
}

impl Default for Blur {
    fn default() -> Self {
        Self::new()
    }
}

impl Blur {
    /// COMPILE_CONTRACT_DESIGN P2: compile both the legacy and Smooth kernels
    /// before a live frame can encounter either mode.
    pub fn prewarm_pipelines(device: &manifold_gpu::GpuDevice) {
        device.create_compute_pipeline(include_str!("shaders/blur.wgsl"), "cs_main", "node.blur");
        device.create_compute_pipeline(
            include_str!("shaders/blur_dual.wgsl"),
            "cs_main",
            "node.blur.smooth",
        );
    }

    fn ensure_dual_pyramid(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        let matches = self
            .dual_pyramid
            .as_ref()
            .is_some_and(|p| p.width == width && p.height == height);
        if matches {
            return;
        }

        let mut down = Vec::with_capacity(DUAL_BLUR_MAX_LEVEL);
        let mut previous_width = width;
        let mut previous_height = height;
        for _ in 1..=DUAL_BLUR_MAX_LEVEL {
            let next_width = (previous_width / 2).max(1);
            let next_height = (previous_height / 2).max(1);
            if next_width == previous_width && next_height == previous_height {
                break;
            }
            down.push(RenderTarget::new(
                device,
                next_width,
                next_height,
                BLUR_SCRATCH_FORMAT,
                "node.blur smooth down",
            ));
            previous_width = next_width;
            previous_height = next_height;
        }

        let mut up_a = Vec::with_capacity(DUAL_BLUR_MAX_LEVEL);
        let mut up_b = Vec::with_capacity(DUAL_BLUR_MAX_LEVEL);
        let mut target_width = width;
        let mut target_height = height;
        for _ in 0..down.len() {
            up_a.push(RenderTarget::new(
                device,
                target_width,
                target_height,
                BLUR_SCRATCH_FORMAT,
                "node.blur smooth up A",
            ));
            up_b.push(RenderTarget::new(
                device,
                target_width,
                target_height,
                BLUR_SCRATCH_FORMAT,
                "node.blur smooth up B",
            ));
            target_width = (target_width / 2).max(1);
            target_height = (target_height / 2).max(1);
        }

        self.dual_pyramid = Some(DualBlurPyramid {
            width,
            height,
            down,
            up_a,
            up_b,
        });
    }
}

impl EffectNode for Blur {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Inherit
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn boundary_reason(&self) -> Option<crate::node_graph::freeze::classify::BoundaryReason> {
        Some(crate::node_graph::freeze::classify::BoundaryReason::BarrieredReduction)
    }
    fn inputs(&self) -> &[NodeInput] {
        &BLUR_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &BLUR_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &BLUR_PARAMS
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let radius = ctx.param_f32("radius", 4.0);

        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(i)) => *i,
            _ => 0,
        };

        let Some(source) = ctx.inputs.texture_2d("source") else {
            return;
        };
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out.width, out.height);

        if mode == DUAL_BLUR_MODE {
            let radius = if radius.is_finite() {
                radius.max(0.0)
            } else {
                0.0
            };
            let gpu = ctx.gpu_encoder();
            if radius > f32::EPSILON {
                self.ensure_dual_pyramid(gpu.device, width, height);
            }
            let pipeline = self.dual_pipeline.get_or_insert_with(|| {
                gpu.device.create_compute_pipeline(
                    include_str!("shaders/blur_dual.wgsl"),
                    "cs_main",
                    "node.blur.smooth",
                )
            });
            let sampler = self
                .sampler
                .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

            let dispatch = |gpu: &mut crate::gpu_encoder::GpuEncoder<'_>,
                            pipeline: &GpuComputePipeline,
                            sampler: &GpuSampler,
                            source: &GpuTexture,
                            source_b: Option<&GpuTexture>,
                            target: &GpuTexture,
                            operation: u32,
                            blend: f32,
                            label: &str| {
                let uniforms = DualBlurUniforms {
                    operation,
                    _pad0: 0,
                    blend,
                    _pad1: 0.0,
                };
                let source_b = source_b.unwrap_or(source);
                let bindings = [
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    GpuBinding::Texture {
                        binding: 1,
                        texture: source,
                    },
                    GpuBinding::Sampler {
                        binding: 2,
                        sampler,
                    },
                    GpuBinding::Texture {
                        binding: 3,
                        texture: target,
                    },
                    GpuBinding::Texture {
                        binding: 4,
                        texture: source_b,
                    },
                ];
                gpu.native_enc.dispatch_compute(
                    pipeline,
                    &bindings,
                    [target.width.div_ceil(16), target.height.div_ceil(16), 1],
                    label,
                );
            };

            if radius <= f32::EPSILON {
                dispatch(
                    gpu,
                    pipeline,
                    sampler,
                    source,
                    None,
                    out,
                    DUAL_BLUR_COPY,
                    0.0,
                    "node.blur (Smooth identity)",
                );
                return;
            }

            let (requested_level, requested_blend) = dual_radius_level(radius);
            let pyramid = self
                .dual_pyramid
                .as_ref()
                .expect("smooth pyramid allocated above");
            let level = requested_level.min(pyramid.down.len());
            let blend = if level < pyramid.down.len() {
                requested_blend
            } else {
                0.0
            };
            let down_count = (level + usize::from(blend > f32::EPSILON)).min(pyramid.down.len());

            let mut current = source;
            for target in pyramid.down.iter().take(down_count) {
                dispatch(
                    gpu,
                    pipeline,
                    sampler,
                    current,
                    None,
                    &target.texture,
                    DUAL_BLUR_DOWN,
                    0.0,
                    "node.blur (Smooth down)",
                );
                current = &target.texture;
            }

            let mut reconstruction_a = source;
            if level > 0 {
                current = &pyramid.down[level - 1].texture;
                for target_level in (1..=level).rev() {
                    let target = &pyramid.up_a[target_level - 1];
                    dispatch(
                        gpu,
                        pipeline,
                        sampler,
                        current,
                        None,
                        &target.texture,
                        DUAL_BLUR_UP,
                        0.0,
                        "node.blur (Smooth up A)",
                    );
                    current = &target.texture;
                }
                reconstruction_a = current;
            }

            let mut reconstruction_b = reconstruction_a;
            if blend > f32::EPSILON {
                current = &pyramid.down[level].texture;
                for target_level in (1..=(level + 1)).rev() {
                    let target = &pyramid.up_b[target_level - 1];
                    dispatch(
                        gpu,
                        pipeline,
                        sampler,
                        current,
                        None,
                        &target.texture,
                        DUAL_BLUR_UP,
                        0.0,
                        "node.blur (Smooth up B)",
                    );
                    current = &target.texture;
                }
                reconstruction_b = current;
            }

            dispatch(
                gpu,
                pipeline,
                sampler,
                reconstruction_a,
                Some(reconstruction_b),
                out,
                DUAL_BLUR_LERP,
                blend,
                "node.blur (Smooth blend)",
            );
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/blur.wgsl"),
                "cs_main",
                "node.blur",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        // (Re)allocate the scratch ping-pong texture if missing or sized wrong.
        let needs_scratch = match &self.scratch {
            Some(s) => s.width != width || s.height != height,
            None => true,
        };
        if needs_scratch {
            self.scratch = Some(RenderTarget::new(
                gpu.device,
                width,
                height,
                BLUR_SCRATCH_FORMAT,
                "node.blur scratch",
            ));
        }
        let scratch_tex = &self
            .scratch
            .as_ref()
            .expect("scratch allocated above")
            .texture;

        // Pass 1: horizontal — source → scratch.
        let uniforms_h = BlurUniforms {
            radius,
            mode,
            direction: [1.0, 0.0],
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms_h),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: source,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: scratch_tex,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.blur (H)",
        );

        // Pass 2: vertical — scratch → out.
        let uniforms_v = BlurUniforms {
            radius,
            mode,
            direction: [0.0, 1.0],
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms_v),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: scratch_tex,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.blur (V)",
        );
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: BLUR_TYPE_ID,
        create: || Box::new(Blur::new()),
        picker: Some(crate::node_graph::palette::PickerInfo { label: "Blur", category: crate::node_graph::palette::PaletteCategory::Atom }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooth_mode_is_appended_without_renumbering_existing_modes() {
        assert_eq!(BLUR_MODES, &["Gaussian", "Box", "Radial", "Smooth"]);
        assert_eq!(DUAL_BLUR_MODE, 3);
    }

    #[test]
    fn dual_radius_interpolation_is_monotonic_and_continuous_at_level_edges() {
        let mut previous = (0usize, 0.0f32);
        for step in 0..=2560 {
            let radius = step as f32 * 0.1;
            let current = dual_radius_level(radius);
            assert!(
                current.0 > previous.0
                    || (current.0 == previous.0 && current.1 + f32::EPSILON >= previous.1),
                "radius {radius} regressed from {previous:?} to {current:?}"
            );
            previous = current;
        }
        assert_eq!(dual_radius_level(0.0), (0, 0.0));
        let (level, blend) = dual_radius_level(64.0);
        assert_eq!(level, 4);
        assert!((blend - (1024.0 - 510.0) / (2046.0 - 510.0)).abs() < 1e-6);
        assert!(dual_radius_level(96.0).0 > level);
        let first_radius = 2.0 * DUAL_BLUR_FIRST_VARIANCE.sqrt();
        let (first_level, first_blend) = dual_radius_level(first_radius);
        assert_eq!(first_level, 1);
        assert!(first_blend < 1e-5);
        assert_eq!(dual_radius_level(first_radius - 0.01).0, 0);
    }

    #[test]
    fn dual_level_variance_matches_gaussian_radius_scale() {
        assert_eq!(dual_level_variance(0), 0.0);
        assert_eq!(dual_level_variance(1), 6.0);
        assert_eq!(dual_level_variance(2), 30.0);
        assert_eq!(dual_level_variance(3), 126.0);
        assert_eq!(dual_level_variance(4), 510.0);
        assert_eq!(dual_level_variance(5), 2046.0);
    }

    #[test]
    fn first_level_variance_is_derived_from_normalized_taps() {
        // For the even 2:1 case, linear sampling at the quarter/three-quarter
        // positions contributes 0.1875 variance in coarse pixels.
        let down_axis_variance = 0.25;
        let up_axis_variance = (4.0 * 0.5f32.powi(2) + 4.0 * 1.5f32.powi(2)) / 8.0 + 0.1875;
        let measured_source_variance = down_axis_variance + 4.0 * up_axis_variance;
        assert!((measured_source_variance - DUAL_BLUR_FIRST_VARIANCE).abs() < 1e-6);
    }

    #[test]
    fn dual_kawase_weights_are_normalized() {
        let down_weight_sum = 4.0 + 4.0;
        let up_weight_sum = 8.0;
        assert_eq!(down_weight_sum / 8.0, 1.0);
        assert_eq!(up_weight_sum / 8.0, 1.0);
        let shader = include_str!("shaders/blur_dual.wgsl");
        assert!(shader.contains("value = value / 8.0"));
        assert!(shader.contains("textureLoad(tex_source, vec2<i32>(px, py), 0)"));
        assert!(shader.contains("mix(a, b"));
        assert!(shader.contains("texture_storage_2d<rgba16float, write>"));
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Production-path proofs for Smooth mode. These build and execute a real
    //! `Source → Blur → FinalOutput` graph, rather than dispatching
    //! `blur_dual.wgsl` directly, so the node's allocation and dispatch logic
    //! is covered as well as the kernel.

    use half::f16;
    use manifold_gpu::{
        GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
        GpuTextureUsage,
    };

    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::Slot;
    use crate::node_graph::execution_plan::{ExecutionPlan, ResourceId};
    use crate::node_graph::{
        Executor, FinalOutput, FrameTime, Graph, MetalBackend, NodeInstanceId, ParamValue, Source,
        compile,
    };
    use crate::render_target::RenderTarget;

    use super::Blur;

    #[derive(Clone, Copy)]
    struct Pixel {
        rgba: [f32; 4],
    }

    fn texture(device: &GpuDevice, width: u32, height: u32, pixels: &[Pixel]) -> GpuTexture {
        assert_eq!(pixels.len(), (width * height) as usize);
        let mut data = Vec::with_capacity(pixels.len() * 4);
        for pixel in pixels {
            data.extend(pixel.rgba.map(f16::from_f32));
        }
        let texture = device.create_texture(&GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::SHADER_WRITE
                | GpuTextureUsage::COPY_SRC,
            label: "blur-smooth-proof-input",
            mip_levels: 1,
        });
        let bytes =
            unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2) };
        device.upload_texture(&texture, bytes);
        texture
    }

    fn readback(device: &GpuDevice, texture: &GpuTexture) -> Vec<[f32; 4]> {
        let row_bytes = texture.width * 8;
        let buffer = device.create_buffer_shared(u64::from(row_bytes) * u64::from(texture.height));
        let mut encoder = device.create_encoder("blur-smooth-proof-readback");
        encoder.copy_texture_to_buffer(texture, &buffer, texture.width, texture.height, row_bytes);
        encoder.commit_and_wait_completed();
        let ptr = buffer.mapped_ptr().expect("blur proof readback buffer");
        let values: &[u16] = unsafe {
            std::slice::from_raw_parts(
                ptr.cast::<u16>(),
                (texture.width * texture.height * 4) as usize,
            )
        };
        values
            .chunks_exact(4)
            .map(|pixel| {
                [
                    f16::from_bits(pixel[0]).to_f32(),
                    f16::from_bits(pixel[1]).to_f32(),
                    f16::from_bits(pixel[2]).to_f32(),
                    f16::from_bits(pixel[3]).to_f32(),
                ]
            })
            .collect()
    }

    fn output_resource(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
        plan.steps()
            .iter()
            .find(|step| step.node == node)
            .and_then(|step| {
                step.outputs
                    .iter()
                    .find(|(name, _)| *name == port)
                    .map(|(_, resource)| *resource)
            })
            .unwrap_or_else(|| panic!("no output {port:?} on node {node:?}"))
    }

    fn run_smooth(
        device: &crate::TestDevice,
        width: u32,
        height: u32,
        radius: f32,
        pixels: &[Pixel],
    ) -> Vec<[f32; 4]> {
        let mut graph = Graph::new();
        let source = graph.add_node(Box::new(Source::new()));
        let blur = graph.add_node(Box::new(Blur::new()));
        let output = graph.add_node(Box::new(FinalOutput::new()));
        graph.set_param(blur, "mode", ParamValue::Enum(3)).unwrap();
        graph
            .set_param(blur, "radius", ParamValue::Float(radius))
            .unwrap();
        graph.connect((source, "out"), (blur, "source")).unwrap();
        graph.connect((blur, "out"), (output, "in")).unwrap();
        let plan = compile(&graph).unwrap();

        let source_resource = output_resource(&plan, source, "out");
        let source_texture = texture(device, width, height, pixels);
        let mut native = device.create_encoder("blur-smooth-proof");
        let mut backend =
            MetalBackend::new(device.arc(), width, height, GpuTextureFormat::Rgba16Float);
        backend.pre_bind_texture_2d(
            source_resource,
            RenderTarget::view_of(source_texture, "blur-smooth-proof-source"),
        );
        let output_slot = Slot(backend.slot_count());
        let mut executor = Executor::new(Box::new(backend));
        {
            let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut native, device);
            executor.execute_frame_with_gpu(
                &mut graph,
                &plan,
                FrameTime {
                    beats: manifold_core::Beats(0.0),
                    seconds: manifold_core::Seconds(0.0),
                    delta: manifold_core::Seconds(1.0 / 60.0),
                    frame_count: 0,
                },
                &mut gpu,
            );
        }
        native.commit_and_wait_completed();
        let output_texture = executor
            .backend()
            .texture_2d(output_slot)
            .expect("blur proof output texture");
        readback(device, output_texture)
    }

    fn solid(width: u32, height: u32, rgba: [f32; 4]) -> Vec<Pixel> {
        vec![Pixel { rgba }; (width * height) as usize]
    }

    fn impulse(width: u32, height: u32) -> Vec<Pixel> {
        let mut pixels = solid(width, height, [0.0; 4]);
        let center = ((height / 2) * width + width / 2) as usize;
        pixels[center] = Pixel {
            rgba: [8.0, 2.0, 0.5, 1.0],
        };
        pixels
    }

    fn impulse_pair(width: u32, height: u32) -> Vec<Pixel> {
        let mut pixels = solid(width, height, [0.0; 4]);
        for (x, y) in [(width / 2, height / 2), (width / 4, height / 4)] {
            pixels[(y * width + x) as usize] = Pixel {
                rgba: [4.0, 1.0, 0.25, 0.5],
            };
        }
        pixels
    }

    fn assert_finite(pixels: &[[f32; 4]]) {
        assert!(pixels.iter().flatten().all(|value| value.is_finite()));
    }

    fn impulse_mass_and_moment(pixels: &[[f32; 4]], width: u32, height: u32) -> (f32, f32) {
        let cx = (width / 2) as f32;
        let cy = (height / 2) as f32;
        let mut mass = 0.0;
        let mut moment = 0.0;
        for y in 0..height {
            for x in 0..width {
                let value = pixels[(y * width + x) as usize][0].max(0.0);
                mass += value;
                moment += value * ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2));
            }
        }
        (mass, moment / mass.max(1e-6))
    }

    #[test]
    fn smooth_production_path_preserves_flat_hdr_and_alpha() {
        let device = crate::test_device();
        let (width, height) = (29, 17);
        let output = run_smooth(
            &device,
            width,
            height,
            32.0,
            &solid(width, height, [6.0, 2.0, 0.5, 0.37]),
        );
        assert_finite(&output);
        for pixel in output {
            assert!((pixel[0] - 6.0).abs() < 0.1);
            assert!((pixel[1] - 2.0).abs() < 0.1);
            assert!((pixel[2] - 0.5).abs() < 0.05);
            assert!((pixel[3] - 0.37).abs() < 0.01);
        }
    }

    #[test]
    fn smooth_radius_zero_is_production_path_identity() {
        let device = crate::test_device();
        let (width, height) = (37, 19);
        let input = (0..width * height)
            .map(|index| {
                let x = index % width;
                let y = index / width;
                Pixel {
                    rgba: [x as f32 * 0.13, y as f32 * 0.17, 3.0, 0.37],
                }
            })
            .collect::<Vec<_>>();
        let output = run_smooth(&device, width, height, 0.0, &input);
        for (index, (expected, actual)) in input.iter().zip(output).enumerate() {
            for (channel, actual_channel) in actual.iter().enumerate() {
                let expected = f16::from_f32(expected.rgba[channel]).to_f32();
                assert_eq!(
                    actual_channel.to_bits(),
                    expected.to_bits(),
                    "texel {index} channel {channel}"
                );
            }
        }
    }

    #[test]
    fn smooth_impulse_has_stable_mass_and_monotonic_second_moment() {
        let device = crate::test_device();
        // Even dimensions keep every pyramid stage at an exact 2:1 ratio,
        // making the measured moment comparable to the calibrated ladder.
        let (width, height) = (512, 512);
        let input = impulse(width, height);
        let radii = [
            0.0, 4.0, 4.89, 4.91, 10.94, 10.97, 22.44, 22.46, 45.16, 45.18, 64.0, 96.0,
        ];
        let mut previous_moment = -f32::EPSILON;
        for radius in radii {
            let output = run_smooth(&device, width, height, radius, &input);
            assert_finite(&output);
            let (mass, moment) = impulse_mass_and_moment(&output, width, height);
            assert!(
                (mass - 8.0).abs() < 0.5,
                "radius {radius} changed mass to {mass}"
            );
            assert!(
                moment + 0.1 >= previous_moment,
                "radius {radius} regressed moment {moment} from {previous_moment}"
            );
            if radius > 64.0 {
                assert!(moment > previous_moment * 1.2,
                    "radius {radius} must broaden the footprint beyond the display range");
            }
            previous_moment = moment;
        }
    }

    #[test]
    fn smooth_odd_large_impulse_stays_finite_and_mass_bounded() {
        let device = crate::test_device();
        let (width, height) = (513, 513);
        let input = impulse_pair(width, height);
        for radius in [32.0, 64.0, 96.0] {
            let output = run_smooth(&device, width, height, radius, &input);
            assert_finite(&output);
            let (mass, moment) = impulse_mass_and_moment(&output, width, height);
            assert!(
                (mass - 8.0).abs() < 1.0,
                "odd pyramid at radius {radius} changed mass to {mass}"
            );
            // This pair probes area coverage at two odd-grid positions. Its
            // combined moment also includes separation and resampling weights;
            // the centred even-grid proof above isolates blur variance.
            assert!(moment.is_finite() && moment > 0.0);
        }
    }

    #[test]
    fn smooth_production_path_handles_odd_and_tiny_dimensions() {
        let device = crate::test_device();
        for (width, height) in [(1, 1), (2, 3), (3, 5), (7, 11), (31, 17)] {
            let input = solid(width, height, [4.0, 1.5, 0.25, 0.63]);
            let output = run_smooth(&device, width, height, 64.0, &input);
            assert_finite(&output);
            assert_eq!(output.len(), (width * height) as usize);
            for pixel in output {
                assert!((pixel[0] - 4.0).abs() < 0.1);
                assert!((pixel[3] - 0.63).abs() < 0.01);
            }
        }
    }
}

// MipChain (node.mip_chain) was a no-op stub for an unbuilt multi-level
// downsample convention; removed when its only consumer (the legacy
// build_bloom composite) was retired. Bloom is now an explicit
// downsample → blur → mix graph in Bloom.json.
