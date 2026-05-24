//! `node.bloom` — pixel-exact replacement for legacy
//! Originally `BloomFX`. Fused composite.
//!
//! Bloom's atomic decomposition would require Unity-style Blur9 tent
//! and Blur13 filmic kernels plus a ping-ponging dual mip chain — a
//! large family of one-off primitives with no obvious reuse outside
//! of bloom-shaped effects. The legacy shader ships as a fused
//! composite primitive instead (same pattern as Glitch, Strobe,
//! EdgeDetect, VoronoiPrism); the future fusion compiler can expose
//! the underlying atoms when there's demand.
//!
//! The primitive owns its mip pyramid state internally (one A chain
//! used for downsamples, one B chain used as the upsample ping-pong
//! buffer). State is rebuilt on size change. `clear_state()` drops
//! the pyramid — wire it on seek/reset paths.

use std::sync::OnceLock;

use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuSampler, GpuSamplerDesc, GpuTextureFormat};

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::node_graph::primitive::PrimitiveDescription;
use crate::render_target::RenderTarget;

// Matches the legacy `BloomFX` constants verbatim. Renaming or
// retuning any of these would break bit-exact parity against the
// existing effect.
const MAX_LEVELS: usize = 6;
const MIN_SIZE: u32 = 16;
const PREFILTER_THRESHOLD: f32 = 0.42;
const PREFILTER_KNEE: f32 = 0.24;
const BLOOM_LEVELS: usize = 3;
const RADIUS_AT_ZERO: f32 = 0.70;
const RADIUS_AT_ONE: f32 = 1.25;

const BLOOM_WGSL: &str = include_str!("shaders/bloom.wgsl");

pub const BLOOM_TYPE_ID: &str = "node.bloom";

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BloomUniforms {
    mode: u32,
    threshold: f32,
    knee: f32,
    intensity: f32,
    radius_scale: f32,
    combine_weight: f32,
    main_texel_size_x: f32,
    main_texel_size_y: f32,
    bloom_texel_size_x: f32,
    bloom_texel_size_y: f32,
    _pad0: f32,
    _pad1: f32,
}

/// Fused-composite Bloom primitive. Owns its mip pyramid.
pub struct Bloom {
    pipeline_prefilter: Option<GpuComputePipeline>,
    pipeline_downsample: Option<GpuComputePipeline>,
    pipeline_upsample: Option<GpuComputePipeline>,
    pipeline_composite: Option<GpuComputePipeline>,
    sampler: Option<GpuSampler>,
    mips_a: Vec<RenderTarget>,
    mips_b: Vec<RenderTarget>,
    pyramid_dims: Option<(u32, u32)>,
}

impl Bloom {
    pub fn new() -> Self {
        Self {
            pipeline_prefilter: None,
            pipeline_downsample: None,
            pipeline_upsample: None,
            pipeline_composite: None,
            sampler: None,
            mips_a: Vec::new(),
            mips_b: Vec::new(),
            pyramid_dims: None,
        }
    }

    fn ensure_pyramid(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.pyramid_dims == Some((width, height)) {
            return;
        }
        let format = GpuTextureFormat::Rgba16Float;
        self.mips_a.clear();
        self.mips_b.clear();
        let mut pw = width.max(1);
        let mut ph = height.max(1);
        for i in 0..MAX_LEVELS {
            if pw < MIN_SIZE || ph < MIN_SIZE {
                break;
            }
            self.mips_a.push(RenderTarget::new(
                device,
                pw,
                ph,
                format,
                &format!("BloomMipA_{i}"),
            ));
            self.mips_b.push(RenderTarget::new(
                device,
                pw,
                ph,
                format,
                &format!("BloomMipB_{i}"),
            ));
            pw = (pw / 2).max(1);
            ph = (ph / 2).max(1);
        }
        self.pyramid_dims = Some((width, height));
    }

    fn ensure_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.pipeline_prefilter.is_none() {
            self.pipeline_prefilter = Some(device.create_specialized_compute_pipeline(
                BLOOM_WGSL,
                "cs_main",
                &[("uniforms.mode", "0u")],
                "node.bloom.prefilter",
            ));
        }
        if self.pipeline_downsample.is_none() {
            self.pipeline_downsample = Some(device.create_specialized_compute_pipeline(
                BLOOM_WGSL,
                "cs_main",
                &[("uniforms.mode", "1u")],
                "node.bloom.downsample",
            ));
        }
        if self.pipeline_upsample.is_none() {
            self.pipeline_upsample = Some(device.create_specialized_compute_pipeline(
                BLOOM_WGSL,
                "cs_main",
                &[("uniforms.mode", "2u")],
                "node.bloom.upsample",
            ));
        }
        if self.pipeline_composite.is_none() {
            self.pipeline_composite = Some(device.create_specialized_compute_pipeline(
                BLOOM_WGSL,
                "cs_main",
                &[("uniforms.mode", "3u")],
                "node.bloom.composite",
            ));
        }
        if self.sampler.is_none() {
            self.sampler = Some(device.create_sampler(&GpuSamplerDesc::default()));
        }
    }
}

impl Default for Bloom {
    fn default() -> Self {
        Self::new()
    }
}

const BLOOM_INPUTS: [NodeInput; 1] = [NodePort {
    name: "in",
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
}];

const BLOOM_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const BLOOM_PARAMS: [ParamDef; 1] = [ParamDef {
    name: "amount",
    label: "Amount",
    ty: ParamType::Float,
    default: ParamValue::Float(0.5),
    range: Some((0.0, 5.0)),
    enum_values: &[],
}];

fn cached_type_id() -> &'static EffectNodeType {
    static CELL: OnceLock<EffectNodeType> = OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(BLOOM_TYPE_ID))
}

impl Bloom {
    /// AI-composition surface metadata (mirrors what the `primitive!`
    /// macro emits for the trivial primitives).
    pub fn description() -> PrimitiveDescription {
        PrimitiveDescription {
            type_id: BLOOM_TYPE_ID,
            purpose: "Energy-conserving HDR bloom. Prefilters bright pixels, downsamples through a mip pyramid with Blur9 (Unity tent) filters, upsamples with Blur13 filmic blends, and composites the bloomed copy back over the source by `amount`.",
            composition_notes: "Fused composite — Unity's Blur9/Blur13 tent and filmic kernels don't decompose into a separable Gaussian library while preserving bit-exact parity. Owns its mip pyramid; rebuilds on size change.",
            examples: &["preset.effect.bloom"],
            inputs: &BLOOM_INPUTS,
            outputs: &BLOOM_OUTPUTS,
            params: &BLOOM_PARAMS,
        }
    }
}

impl EffectNode for Bloom {
    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }
    fn inputs(&self) -> &[NodeInput] {
        &BLOOM_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &BLOOM_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &BLOOM_PARAMS
    }
    fn clear_state(&mut self) {
        self.mips_a.clear();
        self.mips_b.clear();
        self.pyramid_dims = None;
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = match ctx.params.get("amount") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.187,
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
        self.ensure_pyramid(gpu.device, width, height);

        let pp = self.pipeline_prefilter.as_ref().unwrap();
        let pd = self.pipeline_downsample.as_ref().unwrap();
        let pu = self.pipeline_upsample.as_ref().unwrap();
        let pc = self.pipeline_composite.as_ref().unwrap();
        let sampler = self.sampler.as_ref().unwrap();

        // Skip path: pyramid couldn't allocate any levels (input
        // smaller than MIN_SIZE). Run composite mode with zero
        // intensity to copy input → output. Matches the legacy
        // `state.count == 0` branch.
        if self.mips_a.is_empty() {
            let skip_u = BloomUniforms {
                mode: 3,
                threshold: 0.0,
                knee: 0.0,
                intensity: 0.0,
                radius_scale: 1.0,
                combine_weight: 0.0,
                main_texel_size_x: 0.0,
                main_texel_size_y: 0.0,
                bloom_texel_size_x: 0.0,
                bloom_texel_size_y: 0.0,
                _pad0: 0.0,
                _pad1: 0.0,
            };
            dispatch_bloom(
                gpu,
                pc,
                source,
                source,
                target,
                sampler,
                &skip_u,
                width,
                height,
                "node.bloom.skip",
            );
            return;
        }

        let bloom_t = amount.clamp(0.0, 1.0);
        let used_levels = BLOOM_LEVELS.min(self.mips_a.len());
        let t_smooth = bloom_t * bloom_t * (3.0 - 2.0 * bloom_t);
        let radius_scale = RADIUS_AT_ZERO + (RADIUS_AT_ONE - RADIUS_AT_ZERO) * t_smooth;

        let base_uniforms = BloomUniforms {
            mode: 0,
            threshold: PREFILTER_THRESHOLD,
            knee: PREFILTER_KNEE,
            intensity: amount,
            radius_scale,
            combine_weight: 1.0,
            main_texel_size_x: 0.0,
            main_texel_size_y: 0.0,
            bloom_texel_size_x: 0.0,
            bloom_texel_size_y: 0.0,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        let mips_a = &self.mips_a;
        let mips_b = &self.mips_b;

        // Pass 0: Prefilter source → mips_a[0]
        let prefilter_u = BloomUniforms {
            mode: 0,
            main_texel_size_x: 1.0 / width as f32,
            main_texel_size_y: 1.0 / height as f32,
            ..base_uniforms
        };
        dispatch_bloom(
            gpu,
            pp,
            source,
            source,
            &mips_a[0].texture,
            sampler,
            &prefilter_u,
            mips_a[0].width,
            mips_a[0].height,
            "node.bloom.prefilter",
        );

        // Downsample chain: mips_a[i-1] → mips_a[i]
        for i in 1..used_levels {
            let src_w = mips_a[i - 1].width;
            let src_h = mips_a[i - 1].height;
            let down_u = BloomUniforms {
                mode: 1,
                main_texel_size_x: 1.0 / src_w as f32,
                main_texel_size_y: 1.0 / src_h as f32,
                ..base_uniforms
            };
            dispatch_bloom(
                gpu,
                pd,
                &mips_a[i - 1].texture,
                &mips_a[i - 1].texture,
                &mips_a[i].texture,
                sampler,
                &down_u,
                mips_a[i].width,
                mips_a[i].height,
                "node.bloom.downsample",
            );
        }

        // Upsample chain: mips_a[i] + (mips_a or mips_b)[i+1] → mips_b[i]
        for i in (0..used_levels - 1).rev() {
            let hi_w = mips_a[i].width;
            let hi_h = mips_a[i].height;
            let (lo_tex, lo_w, lo_h) = if i == used_levels - 2 {
                (
                    &mips_a[i + 1].texture,
                    mips_a[i + 1].width,
                    mips_a[i + 1].height,
                )
            } else {
                (
                    &mips_b[i + 1].texture,
                    mips_b[i + 1].width,
                    mips_b[i + 1].height,
                )
            };
            let up_u = BloomUniforms {
                mode: 2,
                main_texel_size_x: 1.0 / hi_w as f32,
                main_texel_size_y: 1.0 / hi_h as f32,
                bloom_texel_size_x: 1.0 / lo_w as f32,
                bloom_texel_size_y: 1.0 / lo_h as f32,
                ..base_uniforms
            };
            dispatch_bloom(
                gpu,
                pu,
                &mips_a[i].texture,
                lo_tex,
                &mips_b[i].texture,
                sampler,
                &up_u,
                mips_b[i].width,
                mips_b[i].height,
                "node.bloom.upsample",
            );
        }

        // Final composite: source + Blur13(mips_b[0]) * intensity → target
        let bloom_w = mips_b[0].width;
        let bloom_h = mips_b[0].height;
        let composite_u = BloomUniforms {
            mode: 3,
            main_texel_size_x: 1.0 / width as f32,
            main_texel_size_y: 1.0 / height as f32,
            bloom_texel_size_x: 1.0 / bloom_w as f32,
            bloom_texel_size_y: 1.0 / bloom_h as f32,
            ..base_uniforms
        };
        dispatch_bloom(
            gpu,
            pc,
            source,
            &mips_b[0].texture,
            target,
            sampler,
            &composite_u,
            width,
            height,
            "node.bloom.composite",
        );
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: BLOOM_TYPE_ID,
        create: || Box::new(Bloom::new()),
        picker: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_bloom(
    gpu: &mut crate::gpu_encoder::GpuEncoder<'_>,
    pipeline: &GpuComputePipeline,
    source_a: &manifold_gpu::GpuTexture,
    source_b: &manifold_gpu::GpuTexture,
    target: &manifold_gpu::GpuTexture,
    sampler: &GpuSampler,
    uniforms: &BloomUniforms,
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
