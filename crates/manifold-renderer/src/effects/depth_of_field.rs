// Depth of Field effect — 3 focus modes, full-res separable Gaussian blur.
//
// Modes:
//   0 = Tilt-Shift  — linear focus band with rotation
//   1 = Radial       — circular focus region
//   2 = Depth        — DNN monocular depth map (MiDaS via DepthEstimator.bundle)
//
// Pipeline (4 compute passes, ALL at full resolution):
//   Pass 0: CoC generation (full-res, CoC stored in alpha)
//   Pass 1: Horizontal separable Gaussian blur (full-res, variable width from CoC)
//   Pass 2: Vertical separable Gaussian blur (full-res, variable width from CoC)
//   Pass 3: Composite — blend blurred with sharp original using CoC
//
// Full-res blur is essential: DOF replaces pixels (unlike bloom/halation which
// are additive), so any downsampled intermediate creates visible block artifacts.
// On native Metal / Apple Silicon, full-res separable blur is fast enough.
//
// 6 function-constant-specialized pipelines:
//   3 × CoC variants (tilt-shift, radial, depth) — dead-code eliminate focus_mode
//   1 × H-blur, 1 × V-blur, 1 × composite — dead-code eliminate pass mode
//
// Depth mode spawns its own depth-only BackgroundWorker with MiDaS DNN.
// Workers are shared across all owners (one estimator instance).
// Readback → CPU inference → GPU upload, ~2-3 frame latency.

use std::borrow::Cow;

use super::compute_dual_blit_helper::ComputeDualBlitHelper;
use crate::background_worker::BackgroundWorker;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::gpu_readback::ReadbackRequest;
use crate::node_graph::primitives::DepthOfField;
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
        id: EffectTypeId::DEPTH_OF_FIELD,
        display_name: "Depth of Field",
        category: "Filmic",
        available: true,
        osc_prefix: "dof",
        legacy_discriminant: Some(40),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
            ParamSpec::whole_labels("mode", "Mode", 0.0, 2.0, 0.0, &["Tilt-Shift", "Radial", "Depth"], "Mode"),
            ParamSpec::continuous("focus", "Focus", 0.0, 1.0, 0.5, "F2", "FocusPosition"),
            ParamSpec::continuous("focus_x", "Focus X", 0.0, 1.0, 0.5, "F2", "FocusX"),
            ParamSpec::continuous("width", "Width", 0.01, 0.5, 0.15, "F2", "FocusWidth"),
            ParamSpec::continuous("blur", "Blur", 0.0, 1.0, 0.5, "F2", "BlurStrength"),
            ParamSpec::whole("angle", "Angle", 0.0, 360.0, 0.0, "TiltAngle"),
            ParamSpec::whole_labels("quality", "Quality", 0.0, 2.0, 1.0, &["Low", "Medium", "High"], "Quality"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::DEPTH_OF_FIELD,
        create: |device| Box::new(DepthOfFieldFX::new(device)),
    }
}

fn splice_dof(graph: &mut Graph, source: (NodeInstanceId, &'static str)) -> SpliceResult {
    let node = graph.add_node(Box::new(DepthOfField::new()));
    graph.connect(source, (node, "in")).expect("wire source → DepthOfField.in");
    SpliceResult {
        output: (node, "out"),
        handles: vec![(Cow::Borrowed("dof"), node)],
    }
}

inventory::submit! {
    ChainSpec {
        type_id: EffectTypeId::DEPTH_OF_FIELD,
        splice: splice_dof,
        routings: &[
            Routing { param_id: "amount", target_handle: "dof", target_param: "amount", convert: ParamConvert::Float },
            Routing { param_id: "mode", target_handle: "dof", target_param: "mode", convert: ParamConvert::EnumRound },
            Routing { param_id: "focus", target_handle: "dof", target_param: "focus", convert: ParamConvert::Float },
            Routing { param_id: "focus_x", target_handle: "dof", target_param: "focus_x", convert: ParamConvert::Float },
            Routing { param_id: "width", target_handle: "dof", target_param: "width", convert: ParamConvert::Float },
            Routing { param_id: "blur", target_handle: "dof", target_param: "blur", convert: ParamConvert::Float },
            Routing { param_id: "angle", target_handle: "dof", target_param: "angle", convert: ParamConvert::Float },
            Routing { param_id: "quality", target_handle: "dof", target_param: "quality", convert: ParamConvert::EnumRound },
        ],
        skip: SkipMode::OnZero { param_id: "amount" },
    }
}

// ─── Depth worker types ───────────────────────────────────────────────

struct DofDepthRequest {
    pixel_data: Vec<u8>,
    width: i32,
    height: i32,
}

struct DofDepthResponse {
    depth_buffer: Option<Vec<f32>>,
}

// ─── Constants ────────────────────────────────────────────────────────

const MAX_ANALYSIS_DIM: u32 = 360;
const DEPTH_UPDATE_INTERVAL: i64 = 2; // every 2 frames

// Focus mode indices (must match WGSL uniforms.focus_mode)
const FOCUS_RADIAL: u32 = 1;
const FOCUS_DEPTH: u32 = 2;

// ─── Uniforms ─────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DofUniforms {
    mode: u32,       // pass: 0=CoC+Down, 1=HBlur, 2=VBlur, 3=Composite
    focus_mode: u32, // 0=TiltShift, 1=Radial, 2=Depth
    amount: f32,
    focus_y: f32,
    focus_x: f32,
    focus_width: f32,
    blur_strength: f32,
    tilt_angle: f32, // radians
    quality: u32,    // 0=9tap, 1=17tap, 2=25tap
    texel_size_x: f32,
    texel_size_y: f32,
    _pad: f32,
}

// ─── Per-owner state ──────────────────────────────────────────────────

struct DofOwnerState {
    buf_a: RenderTarget, // blur ping-pong A (full-res)
    buf_b: RenderTarget, // blur ping-pong B (full-res)
}

/// Per-owner state for depth mode (shared across owners via single worker).
struct DepthState {
    analysis_width: u32,
    analysis_height: u32,
    readback: ReadbackRequest,
    readback_pending: bool,
    has_depth: bool,
    depth_dirty: bool,
    depth_buffer: Vec<f32>,
    depth_texture: manifold_gpu::GpuTexture,
    last_request_frame: i64,
}

// ─── Effect ───────────────────────────────────────────────────────────

const DOF_WGSL: &str = include_str!("shaders/fx_depth_of_field_compute.wgsl");

pub struct DepthOfFieldFX {
    helper: ComputeDualBlitHelper,
    // 6 specialized pipelines: 3 CoC variants + blur_h + blur_v + composite
    pipeline_coc_tilt_shift: manifold_gpu::GpuComputePipeline,
    pipeline_coc_radial: manifold_gpu::GpuComputePipeline,
    pipeline_coc_depth: manifold_gpu::GpuComputePipeline,
    pipeline_blur_h: manifold_gpu::GpuComputePipeline,
    pipeline_blur_v: manifold_gpu::GpuComputePipeline,
    pipeline_composite: manifold_gpu::GpuComputePipeline,
    // Per-owner blur state
    states: AHashMap<i64, DofOwnerState>,
    // Depth mode: shared worker + per-owner depth state
    depth_worker: Option<BackgroundWorker<DofDepthRequest, DofDepthResponse>>,
    depth_states: AHashMap<i64, DepthState>,
    depth_worker_tried: bool,
    // Cached dimensions
    width: u32,
    height: u32,
}

impl DepthOfFieldFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        // Specialize pass mode (uniforms.mode) for each pipeline variant.
        // Focus mode (uniforms.focus_mode) is specialized only for CoC pass (mode=0).
        let spec = |mode: &str, focus: Option<&str>, label: &str| {
            let mut constants: Vec<(&str, &str)> = vec![("uniforms.mode", mode)];
            if let Some(f) = focus {
                constants.push(("uniforms.focus_mode", f));
            }
            device.create_specialized_compute_pipeline(DOF_WGSL, "cs_main", &constants, label)
        };

        Self {
            helper: ComputeDualBlitHelper::new(device, DOF_WGSL, "DOF Compute"),
            pipeline_coc_tilt_shift: spec("0u", Some("0u"), "DOF CoC TiltShift"),
            pipeline_coc_radial: spec("0u", Some("1u"), "DOF CoC Radial"),
            pipeline_coc_depth: spec("0u", Some("2u"), "DOF CoC Depth"),
            pipeline_blur_h: spec("1u", None, "DOF HBlur"),
            pipeline_blur_v: spec("2u", None, "DOF VBlur"),
            pipeline_composite: spec("3u", None, "DOF Composite"),
            states: AHashMap::new(),
            depth_worker: None,
            depth_states: AHashMap::new(),
            depth_worker_tried: false,
            width: 0,
            height: 0,
        }
    }

    // ── Blur state management ─────────────────────────────────────────

    fn ensure_blur_state(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        pool: Option<&manifold_gpu::TexturePool>,
        owner_key: i64,
    ) {
        if self.states.contains_key(&owner_key) {
            return;
        }
        if self.width == 0 || self.height == 0 {
            return;
        }
        let format = manifold_gpu::GpuTextureFormat::Rgba16Float;
        let w = self.width;
        let h = self.height;
        let buf_a = if let Some(p) = pool {
            RenderTarget::new_pooled(p, w, h, format, &format!("DofA_{owner_key}"))
        } else {
            RenderTarget::new(device, w, h, format, &format!("DofA_{owner_key}"))
        };
        let buf_b = if let Some(p) = pool {
            RenderTarget::new_pooled(p, w, h, format, &format!("DofB_{owner_key}"))
        } else {
            RenderTarget::new(device, w, h, format, &format!("DofB_{owner_key}"))
        };
        self.states
            .insert(owner_key, DofOwnerState { buf_a, buf_b });
    }

    // ── Depth worker management ───────────────────────────────────────

    fn ensure_depth_worker(&mut self) {
        if self.depth_worker.is_some() || self.depth_worker_tried {
            return;
        }
        self.depth_worker_tried = true;
        self.depth_worker = BackgroundWorker::try_new(|| {
            use manifold_native::depth_estimator::DepthEstimator;
            let mut estimator =
                manifold_native::ffi::depth_ffi::FfiDepthEstimator::new_depth_only()?;
            log::info!("[DepthOfFieldFX] Depth worker spawned (MiDaS depth-only)");
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
            log::warn!("[DepthOfFieldFX] Depth worker unavailable — depth mode disabled");
        }
    }

    fn ensure_depth_state(&mut self, device: &manifold_gpu::GpuDevice, owner_key: i64) {
        if self.depth_states.contains_key(&owner_key) {
            return;
        }
        let scale = (MAX_ANALYSIS_DIM as f32 / self.width.max(self.height) as f32).min(1.0);
        let aw = ((self.width as f32 * scale).round() as u32).max(64);
        let ah = ((self.height as f32 * scale).round() as u32).max(36);
        let pixel_count = (aw * ah) as usize;

        let depth_texture = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: aw,
            height: ah,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL
                | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
            label: &format!("DofDepth_{owner_key}"),
            mip_levels: 1,
        });

        self.depth_states.insert(
            owner_key,
            DepthState {
                analysis_width: aw,
                analysis_height: ah,
                readback: ReadbackRequest::new(),
                readback_pending: false,
                has_depth: false,
                depth_dirty: false,
                depth_buffer: vec![0.0f32; pixel_count],
                depth_texture,
                last_request_frame: -1024,
            },
        );
    }

    /// Start GPU readback of the source texture at analysis resolution.
    /// We render source into a small staging texture, then readback to CPU.
    fn submit_depth_readback(
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        depth_state: &mut DepthState,
    ) {
        // Copy source → analysis-res staging via blit (bilinear downsample)
        // We readback the depth_texture which has CPU_UPLOAD usage
        // but we need a COPY_SRC texture. Create a transient one.
        let aw = depth_state.analysis_width;
        let ah = depth_state.analysis_height;

        // Use the pool for a transient staging texture
        let staging = if let Some(pool) = gpu.pool {
            pool.acquire(
                aw,
                ah,
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
                "DofDepthStaging",
            )
        } else {
            gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: aw,
                height: ah,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
                label: "DofDepthStaging",
                mip_levels: 1,
            })
        };

        // Blit source → staging (bilinear downsample happens automatically)
        gpu.copy_texture_to_texture(source, &staging, aw, ah);

        // Submit readback from staging
        depth_state.readback.submit(gpu, &staging, aw, ah);
        depth_state.readback_pending = true;
    }

    /// Poll readback result, send to depth worker if ready.
    fn poll_depth_readback(
        depth_state: &mut DepthState,
        depth_worker: &mut BackgroundWorker<DofDepthRequest, DofDepthResponse>,
    ) {
        if !depth_state.readback_pending {
            return;
        }
        if let Some(pixels) = depth_state.readback.try_read() {
            depth_state.readback_pending = false;
            depth_worker.submit(DofDepthRequest {
                pixel_data: pixels,
                width: depth_state.analysis_width as i32,
                height: depth_state.analysis_height as i32,
            });
        }
    }

    /// Poll depth worker for completed inference, upload to GPU.
    fn poll_depth_worker(
        depth_state: &mut DepthState,
        depth_worker: &mut BackgroundWorker<DofDepthRequest, DofDepthResponse>,
        gpu: &mut GpuEncoder,
    ) {
        if let Some(response) = depth_worker.try_recv()
            && let Some(depth_buf) = response.depth_buffer
        {
            depth_state.depth_buffer = depth_buf;
            depth_state.has_depth = true;
            depth_state.depth_dirty = true;
        }

        // Upload to GPU if dirty
        if depth_state.depth_dirty {
            let count = (depth_state.analysis_width * depth_state.analysis_height) as usize;
            let mut pixels = vec![0u8; count * 4];
            for i in 0..count {
                let v = (depth_state.depth_buffer[i].clamp(0.0, 1.0) * 255.0) as u8;
                pixels[i * 4] = v;
                pixels[i * 4 + 1] = v;
                pixels[i * 4 + 2] = v;
                pixels[i * 4 + 3] = 255;
            }
            gpu.native_enc.upload_texture(
                &depth_state.depth_texture,
                depth_state.analysis_width,
                depth_state.analysis_height,
                1,
                &pixels,
            );
            depth_state.depth_dirty = false;
        }
    }

    /// Select the CoC pipeline variant based on focus mode.
    fn coc_pipeline(&self, focus_mode: u32) -> &manifold_gpu::GpuComputePipeline {
        match focus_mode {
            FOCUS_RADIAL => &self.pipeline_coc_radial,
            FOCUS_DEPTH => &self.pipeline_coc_depth,
            _ => &self.pipeline_coc_tilt_shift,
        }
    }
}

impl PostProcessEffect for DepthOfFieldFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::DEPTH_OF_FIELD
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        // ── Extract parameters ────────────────────────────────────────
        let amount = fx.param_values.first().map(|p| p.value).unwrap_or(0.0);
        let focus_mode = fx
            .param_values
            .get(1)
            .map(|p| p.value)
            .unwrap_or(0.0)
            .round() as u32;
        let focus_y = fx.param_values.get(2).map(|p| p.value).unwrap_or(0.5);
        let focus_x = fx.param_values.get(3).map(|p| p.value).unwrap_or(0.5);
        let focus_width = fx.param_values.get(4).map(|p| p.value).unwrap_or(0.15);
        let blur_strength = fx.param_values.get(5).map(|p| p.value).unwrap_or(0.5);
        let tilt_angle_deg = fx.param_values.get(6).map(|p| p.value).unwrap_or(0.0);
        let quality = fx
            .param_values
            .get(7)
            .map(|p| p.value)
            .unwrap_or(1.0)
            .round() as u32;
        let tilt_angle = tilt_angle_deg * std::f32::consts::PI / 180.0;

        // ── Ensure state ──────────────────────────────────────────────
        self.width = ctx.width;
        self.height = ctx.height;
        self.ensure_blur_state(gpu.device, gpu.pool, ctx.owner_key);

        if !self.states.contains_key(&ctx.owner_key) {
            return;
        }

        let w = ctx.width;
        let h = ctx.height;

        // ── Depth mode: manage DNN worker + readback ──────────────────
        // Determine which texture to use as source_b for CoC pass
        let depth_tex_for_coc: Option<&manifold_gpu::GpuTexture> = if focus_mode == FOCUS_DEPTH {
            self.ensure_depth_worker();
            self.ensure_depth_state(gpu.device, ctx.owner_key);

            // Borrow split: get depth_worker and depth_state separately
            let has_worker = self.depth_worker.is_some();
            if has_worker {
                // Poll worker for completed results
                let ds = self.depth_states.get_mut(&ctx.owner_key).unwrap();
                let dw = self.depth_worker.as_mut().unwrap();
                Self::poll_depth_readback(ds, dw);
                Self::poll_depth_worker(ds, dw, gpu);

                // Submit new readback if interval elapsed.
                // Guard: reset if frame_count jumped backwards (export restart).
                let ds = self.depth_states.get_mut(&ctx.owner_key).unwrap();
                if ctx.frame_count < ds.last_request_frame {
                    ds.last_request_frame = ctx.frame_count - DEPTH_UPDATE_INTERVAL;
                }
                let elapsed = ctx.frame_count - ds.last_request_frame;
                if elapsed >= DEPTH_UPDATE_INTERVAL && !ds.readback.is_pending() {
                    Self::submit_depth_readback(gpu, source, ds);
                    ds.last_request_frame = ctx.frame_count;
                }
            }

            // Get texture ref if depth data available
            let ds = self.depth_states.get(&ctx.owner_key);
            ds.filter(|d| d.has_depth).map(|d| &d.depth_texture)
        } else {
            None
        };

        // Re-borrow state after depth management
        let state = self.states.get(&ctx.owner_key).unwrap();

        // ── Base uniforms ─────────────────────────────────────────────
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
            texel_size_x: 0.0,
            texel_size_y: 0.0,
            _pad: 0.0,
        };

        // ── Pass 0: CoC generation (full-res) ─────────────────────────
        let texel_x = 1.0 / w as f32;
        let texel_y = 1.0 / h as f32;
        let pass0_u = DofUniforms {
            mode: 0,
            texel_size_x: texel_x,
            texel_size_y: texel_y,
            ..base
        };

        let coc_pipeline = self.coc_pipeline(focus_mode);

        if focus_mode == FOCUS_DEPTH {
            if let Some(depth_tex) = depth_tex_for_coc {
                self.helper.dispatch_with(
                    coc_pipeline,
                    gpu,
                    source,
                    depth_tex,
                    &state.buf_a.texture,
                    bytemuck::bytes_of(&pass0_u),
                    "DOF CoC (Depth)",
                    w,
                    h,
                );
            } else {
                self.helper.dispatch_a_only_with(
                    coc_pipeline,
                    gpu,
                    source,
                    &state.buf_a.texture,
                    bytemuck::bytes_of(&pass0_u),
                    "DOF CoC (Depth pending)",
                    w,
                    h,
                );
            }
        } else {
            self.helper.dispatch_a_only_with(
                coc_pipeline,
                gpu,
                source,
                &state.buf_a.texture,
                bytemuck::bytes_of(&pass0_u),
                "DOF CoC",
                w,
                h,
            );
        }

        // ── Pass 1: Horizontal blur (full-res) ───────────────────────
        let pass1_u = DofUniforms {
            mode: 1,
            texel_size_x: texel_x,
            texel_size_y: texel_y,
            ..base
        };
        self.helper.dispatch_a_only_with(
            &self.pipeline_blur_h,
            gpu,
            &state.buf_a.texture,
            &state.buf_b.texture,
            bytemuck::bytes_of(&pass1_u),
            "DOF HBlur",
            w,
            h,
        );

        // ── Pass 2: Vertical blur (full-res) ─────────────────────────
        let pass2_u = DofUniforms {
            mode: 2,
            texel_size_x: texel_x,
            texel_size_y: texel_y,
            ..base
        };
        self.helper.dispatch_a_only_with(
            &self.pipeline_blur_v,
            gpu,
            &state.buf_b.texture,
            &state.buf_a.texture,
            bytemuck::bytes_of(&pass2_u),
            "DOF VBlur",
            w,
            h,
        );

        // ── Pass 3: Composite (full-res) ─────────────────────────────
        let pass3_u = DofUniforms {
            mode: 3,
            texel_size_x: texel_x,
            texel_size_y: texel_y,
            ..base
        };
        self.helper.dispatch_with(
            &self.pipeline_composite,
            gpu,
            source,
            &state.buf_a.texture,
            target,
            bytemuck::bytes_of(&pass3_u),
            "DOF Composite",
            w,
            h,
        );
    }

    fn clear_state(&mut self) {
        self.states.clear();
        self.depth_states.clear();
    }

    fn resize(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        let format = manifold_gpu::GpuTextureFormat::Rgba16Float;
        for (key, state) in &mut self.states {
            state.buf_a = RenderTarget::new(device, width, height, format, &format!("DofA_{key}"));
            state.buf_b = RenderTarget::new(device, width, height, format, &format!("DofB_{key}"));
        }
        // Depth states use analysis-res which may also change
        self.depth_states.clear();
    }

    fn flush_background_work(&mut self) {
        if let Some(ref mut worker) = self.depth_worker {
            let _ = worker.recv_blocking();
        }
    }
}
