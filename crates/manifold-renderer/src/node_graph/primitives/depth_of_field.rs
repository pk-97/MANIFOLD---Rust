//! `primitive.depth_of_field` — pixel-exact replacement for legacy
//! [`DepthOfFieldFX`](crate::effects::depth_of_field::DepthOfFieldFX).
//! Fused composite with three focus modes:
//!
//! * **Tilt-Shift** (`focus_mode = 0`) — linear focus band with rotation.
//! * **Radial** (`focus_mode = 1`) — circular focus region.
//! * **Depth** (`focus_mode = 2`) — MiDaS monocular depth map driving CoC.
//!
//! Four-pass pipeline (all at full resolution):
//!
//! ```text
//!   source → [Pass 0: CoC]   → buf_a
//!            [Pass 1: HBlur] → buf_b
//!            [Pass 2: VBlur] → buf_a
//!            [Pass 3: Composite] source + buf_a → target
//! ```
//!
//! Depth mode optionally drives a background MiDaS worker that reads
//! back the source at a small analysis resolution, runs inference on a
//! CPU thread, and uploads the depth result back as a texture. Until
//! the first inference completes, the CoC pass runs with the source
//! itself as `source_b` (legacy fallback) — visually identical to
//! tilt-shift centred on the source's red channel, but matches the
//! legacy behaviour bit-for-bit. ~2–3 frame inference latency.
//!
//! The design doc originally proposed splitting tilt-shift / radial
//! into atomic primitives (CoC + variable-width Gaussian + composite)
//! while keeping depth monolithic. The geometric variants reach the
//! same CoC-modulated Gaussian as the depth path, just with a different
//! CoC source — so a fused primitive with three CoC pipelines and one
//! shared blur+composite path is simpler and avoids duplicating the
//! variable-width blur primitive that doesn't exist yet. The shader is
//! shared with `effects/shaders/fx_depth_of_field_compute.wgsl` via
//! `include_str!` until §6.6 cutover deletes the legacy effect.

use std::sync::OnceLock;

use manifold_gpu::{
    GpuBinding, GpuComputePipeline, GpuSampler, GpuSamplerDesc, GpuTexture, GpuTextureDesc,
    GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use manifold_native::depth_estimator::DepthEstimator;

use crate::background_worker::BackgroundWorker;
use crate::gpu_readback::ReadbackRequest;
use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::node_graph::primitive::PrimitiveDescription;
use crate::render_target::RenderTarget;

const DOF_WGSL: &str = include_str!("../../effects/shaders/fx_depth_of_field_compute.wgsl");

pub const DEPTH_OF_FIELD_TYPE_ID: &str = "primitive.depth_of_field";

pub const DEPTH_OF_FIELD_FOCUS_MODES: &[&str] = &["Tilt-Shift", "Radial", "Depth"];
pub const DEPTH_OF_FIELD_QUALITIES: &[&str] = &["Low", "Medium", "High"];

const FOCUS_RADIAL: u32 = 1;
const FOCUS_DEPTH: u32 = 2;
const MAX_ANALYSIS_DIM: u32 = 360;
const DEPTH_UPDATE_INTERVAL: i64 = 2;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DofUniforms {
    mode: u32,
    focus_mode: u32,
    amount: f32,
    focus_y: f32,
    focus_x: f32,
    focus_width: f32,
    blur_strength: f32,
    tilt_angle: f32,
    quality: u32,
    texel_size_x: f32,
    texel_size_y: f32,
    _pad: f32,
}

struct DofDepthRequest {
    pixel_data: Vec<u8>,
    width: i32,
    height: i32,
}

struct DofDepthResponse {
    depth_buffer: Option<Vec<f32>>,
}

struct DepthState {
    analysis_width: u32,
    analysis_height: u32,
    readback: ReadbackRequest,
    readback_pending: bool,
    has_depth: bool,
    depth_dirty: bool,
    depth_buffer: Vec<f32>,
    depth_texture: GpuTexture,
    last_request_frame: i64,
    frame_counter: i64,
}

/// Fused-composite DoF primitive. Owns its ping-pong textures and
/// the optional MiDaS background worker.
pub struct DepthOfField {
    pipeline_coc_tilt_shift: Option<GpuComputePipeline>,
    pipeline_coc_radial: Option<GpuComputePipeline>,
    pipeline_coc_depth: Option<GpuComputePipeline>,
    pipeline_blur_h: Option<GpuComputePipeline>,
    pipeline_blur_v: Option<GpuComputePipeline>,
    pipeline_composite: Option<GpuComputePipeline>,
    sampler: Option<GpuSampler>,
    buf_a: Option<RenderTarget>,
    buf_b: Option<RenderTarget>,
    buf_dims: Option<(u32, u32)>,
    depth_worker: Option<BackgroundWorker<DofDepthRequest, DofDepthResponse>>,
    depth_worker_tried: bool,
    depth_state: Option<DepthState>,
}

impl DepthOfField {
    pub fn new() -> Self {
        Self {
            pipeline_coc_tilt_shift: None,
            pipeline_coc_radial: None,
            pipeline_coc_depth: None,
            pipeline_blur_h: None,
            pipeline_blur_v: None,
            pipeline_composite: None,
            sampler: None,
            buf_a: None,
            buf_b: None,
            buf_dims: None,
            depth_worker: None,
            depth_worker_tried: false,
            depth_state: None,
        }
    }

    fn ensure_buffers(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.buf_dims == Some((width, height)) {
            return;
        }
        let format = GpuTextureFormat::Rgba16Float;
        self.buf_a = Some(RenderTarget::new(device, width, height, format, "DofA"));
        self.buf_b = Some(RenderTarget::new(device, width, height, format, "DofB"));
        self.buf_dims = Some((width, height));
    }

    fn ensure_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        let spec = |mode: &str, focus: Option<&str>, label: &str| {
            let mut constants: Vec<(&str, &str)> = vec![("uniforms.mode", mode)];
            if let Some(f) = focus {
                constants.push(("uniforms.focus_mode", f));
            }
            device.create_specialized_compute_pipeline(DOF_WGSL, "cs_main", &constants, label)
        };
        if self.pipeline_coc_tilt_shift.is_none() {
            self.pipeline_coc_tilt_shift =
                Some(spec("0u", Some("0u"), "primitive.dof.coc.tilt_shift"));
        }
        if self.pipeline_coc_radial.is_none() {
            self.pipeline_coc_radial =
                Some(spec("0u", Some("1u"), "primitive.dof.coc.radial"));
        }
        if self.pipeline_coc_depth.is_none() {
            self.pipeline_coc_depth =
                Some(spec("0u", Some("2u"), "primitive.dof.coc.depth"));
        }
        if self.pipeline_blur_h.is_none() {
            self.pipeline_blur_h = Some(spec("1u", None, "primitive.dof.blur_h"));
        }
        if self.pipeline_blur_v.is_none() {
            self.pipeline_blur_v = Some(spec("2u", None, "primitive.dof.blur_v"));
        }
        if self.pipeline_composite.is_none() {
            self.pipeline_composite = Some(spec("3u", None, "primitive.dof.composite"));
        }
        if self.sampler.is_none() {
            self.sampler = Some(device.create_sampler(&GpuSamplerDesc::default()));
        }
    }

    fn ensure_depth_worker(&mut self) {
        if self.depth_worker.is_some() || self.depth_worker_tried {
            return;
        }
        self.depth_worker_tried = true;
        self.depth_worker = BackgroundWorker::try_new(|| {
            let mut estimator =
                manifold_native::ffi::depth_ffi::FfiDepthEstimator::new_depth_only()?;
            log::info!("[primitive.depth_of_field] Depth worker spawned (MiDaS depth-only)");
            Some(move |req: DofDepthRequest| -> DofDepthResponse {
                let pc = (req.width * req.height) as usize;
                let mut depth = vec![0f32; pc];
                let ok = estimator.process(
                    &req.pixel_data,
                    req.width,
                    req.height,
                    &mut depth,
                    req.width,
                    req.height,
                );
                DofDepthResponse {
                    depth_buffer: if ok != 0 { Some(depth) } else { None },
                }
            })
        });
        if self.depth_worker.is_none() {
            log::warn!(
                "[primitive.depth_of_field] Depth worker unavailable — depth mode disabled"
            );
        }
    }

    fn ensure_depth_state(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.depth_state.is_some() {
            return;
        }
        let scale = (MAX_ANALYSIS_DIM as f32 / width.max(height) as f32).min(1.0);
        let aw = ((width as f32 * scale).round() as u32).max(64);
        let ah = ((height as f32 * scale).round() as u32).max(36);
        let pixel_count = (aw * ah) as usize;

        let depth_texture = device.create_texture(&GpuTextureDesc {
            width: aw,
            height: ah,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
            label: "primitive.dof.depth",
            mip_levels: 1,
        });

        self.depth_state = Some(DepthState {
            analysis_width: aw,
            analysis_height: ah,
            readback: ReadbackRequest::new(),
            readback_pending: false,
            has_depth: false,
            depth_dirty: false,
            depth_buffer: vec![0.0f32; pixel_count],
            depth_texture,
            last_request_frame: -1024,
            frame_counter: 0,
        });
    }
}

impl Default for DepthOfField {
    fn default() -> Self {
        Self::new()
    }
}

const DOF_INPUTS: [NodeInput; 1] = [NodePort {
    name: "in",
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
}];

const DOF_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const DOF_PARAMS: [ParamDef; 8] = [
    ParamDef {
        name: "amount",
        label: "Amount",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "mode",
        label: "Mode",
        ty: ParamType::Enum,
        default: ParamValue::Enum(0),
        range: Some((0.0, 2.0)),
        enum_values: DEPTH_OF_FIELD_FOCUS_MODES,
    },
    ParamDef {
        name: "focus",
        label: "Focus",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "focus_x",
        label: "Focus X",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "width",
        label: "Width",
        ty: ParamType::Float,
        default: ParamValue::Float(0.15),
        range: Some((0.01, 0.5)),
        enum_values: &[],
    },
    ParamDef {
        name: "blur",
        label: "Blur",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "angle",
        label: "Angle",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((0.0, 360.0)),
        enum_values: &[],
    },
    ParamDef {
        name: "quality",
        label: "Quality",
        ty: ParamType::Enum,
        default: ParamValue::Enum(1),
        range: Some((0.0, 2.0)),
        enum_values: DEPTH_OF_FIELD_QUALITIES,
    },
];

fn cached_type_id() -> &'static EffectNodeType {
    static CELL: OnceLock<EffectNodeType> = OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(DEPTH_OF_FIELD_TYPE_ID))
}

impl DepthOfField {
    pub fn description() -> PrimitiveDescription {
        PrimitiveDescription {
            type_id: DEPTH_OF_FIELD_TYPE_ID,
            purpose: "CoC-modulated depth of field with tilt-shift, radial, or MiDaS-depth focus modes. Four-pass pipeline: CoC → H Gaussian (variable-width) → V Gaussian → composite back over the sharp source.",
            composition_notes: "Fused composite — variable-width Gaussian and depth-DNN orchestration don't decompose into atomic primitives without breaking parity. Depth mode spawns a background MiDaS worker on first dispatch; geometric modes are pure GPU.",
            examples: &["preset.effect.depth_of_field"],
            inputs: &DOF_INPUTS,
            outputs: &DOF_OUTPUTS,
            params: &DOF_PARAMS,
        }
    }
}

impl EffectNode for DepthOfField {
    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }
    fn inputs(&self) -> &[NodeInput] {
        &DOF_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &DOF_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &DOF_PARAMS
    }
    fn clear_state(&mut self) {
        self.buf_a = None;
        self.buf_b = None;
        self.buf_dims = None;
        self.depth_state = None;
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = read_f32(ctx, "amount", 0.0);
        let focus_mode = read_u32(ctx, "mode", 0).min(2);
        let focus_y = read_f32(ctx, "focus", 0.5);
        let focus_x = read_f32(ctx, "focus_x", 0.5);
        let focus_width = read_f32(ctx, "width", 0.15);
        let blur_strength = read_f32(ctx, "blur", 0.5);
        let tilt_angle_deg = read_f32(ctx, "angle", 0.0);
        let quality = read_u32(ctx, "quality", 1).min(2);
        let tilt_angle = tilt_angle_deg * std::f32::consts::PI / 180.0;

        let Some(source) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (target.width, target.height);

        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("primitive.depth_of_field requires a GpuEncoder");
        self.ensure_pipelines(gpu.device);
        self.ensure_buffers(gpu.device, width, height);

        // Depth-mode bookkeeping. We track an internal frame counter so
        // the readback cadence matches the legacy (every
        // `DEPTH_UPDATE_INTERVAL` frames). One primitive instance =
        // one logical owner; resets on `clear_state`.
        let depth_tex_for_coc: Option<&GpuTexture> = if focus_mode == FOCUS_DEPTH {
            self.ensure_depth_worker();
            self.ensure_depth_state(gpu.device, width, height);
            if let (Some(ds), Some(dw)) =
                (self.depth_state.as_mut(), self.depth_worker.as_mut())
            {
                // Poll readback → submit to worker.
                if ds.readback_pending
                    && let Some(pixels) = ds.readback.try_read()
                {
                    ds.readback_pending = false;
                    dw.submit(DofDepthRequest {
                        pixel_data: pixels,
                        width: ds.analysis_width as i32,
                        height: ds.analysis_height as i32,
                    });
                }

                // Poll worker result → upload to GPU.
                if let Some(response) = dw.try_recv()
                    && let Some(buf) = response.depth_buffer
                {
                    ds.depth_buffer = buf;
                    ds.has_depth = true;
                    ds.depth_dirty = true;
                }
                if ds.depth_dirty {
                    let count = (ds.analysis_width * ds.analysis_height) as usize;
                    let mut pixels = vec![0u8; count * 4];
                    for i in 0..count {
                        let v = (ds.depth_buffer[i].clamp(0.0, 1.0) * 255.0) as u8;
                        pixels[i * 4] = v;
                        pixels[i * 4 + 1] = v;
                        pixels[i * 4 + 2] = v;
                        pixels[i * 4 + 3] = 255;
                    }
                    gpu.native_enc.upload_texture(
                        &ds.depth_texture,
                        ds.analysis_width,
                        ds.analysis_height,
                        1,
                        &pixels,
                    );
                    ds.depth_dirty = false;
                }

                // Submit fresh readback every DEPTH_UPDATE_INTERVAL frames.
                let elapsed = ds.frame_counter - ds.last_request_frame;
                if elapsed >= DEPTH_UPDATE_INTERVAL && !ds.readback.is_pending() {
                    let aw = ds.analysis_width;
                    let ah = ds.analysis_height;
                    let staging = gpu.device.create_texture(&GpuTextureDesc {
                        width: aw,
                        height: ah,
                        depth: 1,
                        format: GpuTextureFormat::Rgba16Float,
                        dimension: GpuTextureDimension::D2,
                        usage: GpuTextureUsage::RENDER_TARGET_FULL,
                        label: "primitive.dof.depth.staging",
                        mip_levels: 1,
                    });
                    gpu.copy_texture_to_texture(source, &staging, aw, ah);
                    ds.readback.submit(gpu, &staging, aw, ah);
                    ds.readback_pending = true;
                    ds.last_request_frame = ds.frame_counter;
                }
                ds.frame_counter += 1;

                if ds.has_depth {
                    Some(&ds.depth_texture)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        let sampler = self.sampler.as_ref().unwrap();
        let pipeline_coc = match focus_mode {
            FOCUS_RADIAL => self.pipeline_coc_radial.as_ref().unwrap(),
            FOCUS_DEPTH => self.pipeline_coc_depth.as_ref().unwrap(),
            _ => self.pipeline_coc_tilt_shift.as_ref().unwrap(),
        };
        let pipeline_blur_h = self.pipeline_blur_h.as_ref().unwrap();
        let pipeline_blur_v = self.pipeline_blur_v.as_ref().unwrap();
        let pipeline_composite = self.pipeline_composite.as_ref().unwrap();

        let texel_x = 1.0 / width as f32;
        let texel_y = 1.0 / height as f32;
        let base = DofUniforms {
            mode: 0,
            focus_mode,
            amount,
            focus_y,
            focus_x,
            focus_width,
            blur_strength,
            tilt_angle,
            quality,
            texel_size_x: texel_x,
            texel_size_y: texel_y,
            _pad: 0.0,
        };

        // Pass 0: CoC → buf_a
        let pass0_u = DofUniforms { mode: 0, ..base };
        if focus_mode == FOCUS_DEPTH {
            if let Some(depth_tex) = depth_tex_for_coc {
                dispatch_dof(
                    gpu,
                    pipeline_coc,
                    source,
                    depth_tex,
                    &buf_a.texture,
                    sampler,
                    &pass0_u,
                    width,
                    height,
                    "primitive.dof.coc.depth",
                );
            } else {
                dispatch_dof(
                    gpu,
                    pipeline_coc,
                    source,
                    source,
                    &buf_a.texture,
                    sampler,
                    &pass0_u,
                    width,
                    height,
                    "primitive.dof.coc.depth_pending",
                );
            }
        } else {
            dispatch_dof(
                gpu,
                pipeline_coc,
                source,
                source,
                &buf_a.texture,
                sampler,
                &pass0_u,
                width,
                height,
                "primitive.dof.coc",
            );
        }

        // Pass 1: H blur, buf_a → buf_b
        let pass1_u = DofUniforms { mode: 1, ..base };
        dispatch_dof(
            gpu,
            pipeline_blur_h,
            &buf_a.texture,
            &buf_a.texture,
            &buf_b.texture,
            sampler,
            &pass1_u,
            width,
            height,
            "primitive.dof.blur_h",
        );

        // Pass 2: V blur, buf_b → buf_a
        let pass2_u = DofUniforms { mode: 2, ..base };
        dispatch_dof(
            gpu,
            pipeline_blur_v,
            &buf_b.texture,
            &buf_b.texture,
            &buf_a.texture,
            sampler,
            &pass2_u,
            width,
            height,
            "primitive.dof.blur_v",
        );

        // Pass 3: Composite source + buf_a → target
        let pass3_u = DofUniforms { mode: 3, ..base };
        dispatch_dof(
            gpu,
            pipeline_composite,
            source,
            &buf_a.texture,
            target,
            sampler,
            &pass3_u,
            width,
            height,
            "primitive.dof.composite",
        );
    }
}

fn read_f32(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
    match ctx.params.get(name) {
        Some(ParamValue::Float(f)) => *f,
        _ => default,
    }
}

fn read_u32(ctx: &EffectNodeContext<'_, '_>, name: &str, default: u32) -> u32 {
    match ctx.params.get(name) {
        Some(ParamValue::Enum(v)) => *v,
        Some(ParamValue::Float(f)) => f.round() as u32,
        _ => default,
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_dof(
    gpu: &mut crate::gpu_encoder::GpuEncoder<'_>,
    pipeline: &GpuComputePipeline,
    source_a: &GpuTexture,
    source_b: &GpuTexture,
    target: &GpuTexture,
    sampler: &GpuSampler,
    uniforms: &DofUniforms,
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

