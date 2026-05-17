// Mechanical port of WireframeDepthFX.cs + WireframeDepthEffect.shader.
// Unity source: Assets/Scripts/Compositing/Effects/WireframeDepthFX.cs (1094 lines)
//              Assets/Shaders/WireframeDepthEffect.shader (15 passes)
//              Assets/Scripts/Compositing/Effects/DepthEstimatorNative.cs
//
// Same logic, same variables, same constants, same edge cases.
// AsyncGPUReadback → poll-based ReadbackRequest (submit + try_read).
// Time.frameCount → EffectContext.frame_count.
// Graphics.Blit → compute dispatch per pass.

use crate::background_worker::BackgroundWorker;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::gpu_readback::ReadbackRequest;
use crate::node_graph::primitives::WireframeDepth;
use crate::node_graph::{ParamBinding, ParamConvert, ParamTarget, SkipMode};
use crate::render_target::RenderTarget;
use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;
use std::borrow::Cow;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::WIREFRAME_DEPTH,
        display_name: "Wireframe Depth",
        category: "Diagnostic",
        available: true,
        osc_prefix: "wireframeDepth",
        legacy_discriminant: Some(29),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),
            ParamSpec::continuous("density", "Density", 16.0, 280.0, 260.0, "F2", "Density"),
            ParamSpec::continuous("width", "Width", 0.4, 3.0, 1.5, "F2", "Width"),
            ParamSpec::continuous("z_scale", "Z Scale", 0.0, 2.5, 1.35, "F2", "ZScale"),
            ParamSpec::continuous("smooth", "Smooth", 0.0, 1.0, 0.90, "F2", "Smooth"),
            ParamSpec::continuous("subject", "Subject", 0.0, 1.0, 0.5, "F2", "SubjectIsolation"),
            ParamSpec::whole_labels("blend", "Blend", 0.0, 6.0, 6.0, &["Normal", "Add", "Multiply", "Screen", "Overlay", "Stencil", "Opaque"], "BlendMode"),
            ParamSpec::continuous("wire_res", "Wire Resolution", 0.5, 1.0, 1.0, "F2", "WireRes"),
            ParamSpec::whole_labels("mesh_rate", "Mesh Rate", 1.0, 4.0, 1.0, &["Every", "Half", "Third", "Quarter"], "MeshRate"),
            ParamSpec::whole_labels("flow", "Flow", 0.0, 1.0, 1.0, &["Off", "On"], "NativeFlow"),
            ParamSpec::whole_labels("lock", "Lock", 0.0, 1.0, 1.0, &["Off", "On"], "FlowLock"),
            ParamSpec::continuous("edge_follow", "Edge Follow", 0.0, 1.0, 0.5, "F2", "EdgeFollow"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::WIREFRAME_DEPTH,
        create: |device| Box::new(WireframeDepthFX::new(device)),
    }
}

crate::atomic_chain_spec! {
    type_id: EffectTypeId::WIREFRAME_DEPTH,
    primitive: WireframeDepth,
    handle: "wireframe_depth",
    bindings: &[
        ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 1.0,
            target: ParamTarget::HandleNode { handle: "wireframe_depth", param: "amount" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("density"),
            label: "Density",
            default_value: 260.0,
            target: ParamTarget::HandleNode { handle: "wireframe_depth", param: "density" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("width"),
            label: "Width",
            default_value: 1.5,
            target: ParamTarget::HandleNode { handle: "wireframe_depth", param: "width" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("z_scale"),
            label: "Z Scale",
            default_value: 1.35,
            target: ParamTarget::HandleNode { handle: "wireframe_depth", param: "z_scale" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("smooth"),
            label: "Smooth",
            default_value: 0.90,
            target: ParamTarget::HandleNode { handle: "wireframe_depth", param: "smooth" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("subject"),
            label: "Subject",
            default_value: 0.5,
            target: ParamTarget::HandleNode { handle: "wireframe_depth", param: "subject" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("blend"),
            label: "Blend",
            default_value: 6.0,
            target: ParamTarget::HandleNode { handle: "wireframe_depth", param: "blend" },
            convert: ParamConvert::EnumRound,
        },
        ParamBinding {
            id: Cow::Borrowed("wire_res"),
            label: "Wire Resolution",
            default_value: 1.0,
            target: ParamTarget::HandleNode { handle: "wireframe_depth", param: "wire_res" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("mesh_rate"),
            label: "Mesh Rate",
            default_value: 1.0,
            target: ParamTarget::HandleNode { handle: "wireframe_depth", param: "mesh_rate" },
            convert: ParamConvert::EnumRound,
        },
        ParamBinding {
            id: Cow::Borrowed("flow"),
            label: "Flow",
            default_value: 1.0,
            target: ParamTarget::HandleNode { handle: "wireframe_depth", param: "flow" },
            convert: ParamConvert::EnumRound,
        },
        ParamBinding {
            id: Cow::Borrowed("lock"),
            label: "Lock",
            default_value: 1.0,
            target: ParamTarget::HandleNode { handle: "wireframe_depth", param: "lock" },
            convert: ParamConvert::EnumRound,
        },
        ParamBinding {
            id: Cow::Borrowed("edge_follow"),
            label: "Edge Follow",
            default_value: 0.5,
            target: ParamTarget::HandleNode { handle: "wireframe_depth", param: "edge_follow" },
            convert: ParamConvert::Float,
        },
    ],
    // Stateful: 15-pass pipeline + 3 DNN worker threads + temporal
    // smoothing buffers. SkipMode::OnZero would tear all of that
    // down on every amount → 0 drag, paying many hundreds of ms of
    // worker spin-up on the way back. Always splice — at `amount = 0`
    // the inner composite returns the source.
    skip: SkipMode::Never,
}

// Request/response types for the background depth estimation worker.
struct DepthRequest {
    owner_key: i64,
    pixel_data: Vec<u8>,
    prev_pixel_data: Vec<u8>,
    has_prev_frame: bool,
    width: i32,
    height: i32,
    wants_flow: bool,
    wants_depth: bool,
    wants_subject: bool,
    has_subject_mask_history: bool,
    subject_history: Vec<f32>,
}

struct DepthResponse {
    owner_key: i64,
    flow_buffer: Option<Vec<f32>>,
    cut_score: f32,
    depth_buffer: Option<Vec<f32>>,
    subject_history_blended: Option<Vec<f32>>,
    subject_api_failed: bool,
}

// Per-task request/response types for parallel worker mode.
struct DepthOnlyRequest {
    owner_key: i64,
    pixel_data: Vec<u8>,
    width: i32,
    height: i32,
}
struct DepthOnlyResponse {
    owner_key: i64,
    depth_buffer: Option<Vec<f32>>,
}

struct FlowOnlyRequest {
    owner_key: i64,
    pixel_data: Vec<u8>,
    prev_pixel_data: Vec<u8>,
    has_prev_frame: bool,
    width: i32,
    height: i32,
}
struct FlowOnlyResponse {
    owner_key: i64,
    flow_buffer: Option<Vec<f32>>,
    cut_score: f32,
}

struct SubjectOnlyRequest {
    owner_key: i64,
    pixel_data: Vec<u8>,
    width: i32,
    height: i32,
    has_subject_mask_history: bool,
    subject_history: Vec<f32>,
}
struct SubjectOnlyResponse {
    owner_key: i64,
    subject_history_blended: Option<Vec<f32>>,
    subject_api_failed: bool,
}

enum WorkerMode {
    Parallel {
        depth_worker: BackgroundWorker<DepthOnlyRequest, DepthOnlyResponse>,
        flow_worker: BackgroundWorker<FlowOnlyRequest, FlowOnlyResponse>,
        subject_worker: BackgroundWorker<SubjectOnlyRequest, SubjectOnlyResponse>,
    },
    Monolithic {
        worker: BackgroundWorker<DepthRequest, DepthResponse>,
    },
}

// WireframeDepthFX.cs line 21-35
const PASS_ANALYSIS: usize = 0;
const PASS_WIREFRAME_MASK: usize = 1;
const PASS_UPDATE_HISTORY: usize = 2;
const PASS_COMPOSITE: usize = 3;
const PASS_DNN_DEPTH_POST: usize = 4;
const PASS_FLOW_ESTIMATE: usize = 5;
const PASS_FLOW_ADVECT_COORD: usize = 6;
const PASS_INIT_MESH_COORD: usize = 7;
const PASS_MESH_REGULARIZE: usize = 8;
const PASS_MESH_CELL_AFFINE: usize = 9;
const PASS_SEMANTIC_MASK: usize = 10;
const PASS_MESH_FACE_WARP: usize = 11;
const PASS_SURFACE_CACHE_UPDATE: usize = 12;
const PASS_FLOW_HYGIENE: usize = 13;

// WireframeDepthFX.cs line 36-39
const MAX_ANALYSIS_DIM: u32 = 360;
const NATIVE_UPDATE_INTERVAL_DNN: i64 = 1;
const NATIVE_UPDATE_INTERVAL_SUBJECT: i64 = 1;

// WireframeDepthFX.cs line 47-90 — OwnerState
// ARGB32  → Rgba8Unorm
// ARGBHalf → Rgba16Float
// RGBAFloat (nativeFlowTexture) → Rgba16Float (Metal: Rgba32Float not filterable;
//   see KNOWN_DIVERGENCES)
struct OwnerState {
    analysis_width: u32,
    analysis_height: u32,
    wire_width: u32,
    wire_height: u32,
    // RenderTextures
    previous_analysis_tex: RenderTarget, // ARGB32 → Rgba8Unorm
    depth_tex: RenderTarget,             // ARGBHalf → Rgba16Float
    line_history_tex: RenderTarget,      // ARGB32 → Rgba8Unorm
    flow_tex: RenderTarget,              // ARGBHalf → Rgba16Float
    mesh_coord_tex: RenderTarget,        // ARGBHalf → Rgba16Float
    semantic_tex: RenderTarget,          // ARGBHalf → Rgba16Float
    surface_cache_tex: RenderTarget,     // ARGBHalf → Rgba16Float
    dnn_input_tex: RenderTarget,         // Rgba8Unorm (matches analysis_rt for blit copy)
    // DNN depth CPU path
    dnn_readback_pending: bool,
    dnn_has_depth: bool,
    dnn_depth_dirty: bool,
    _dnn_pixel_buffer: Vec<u8>, // byte[analysisWidth * analysisHeight * 4]
    dnn_depth_buffer: Vec<f32>, // float[analysisWidth * analysisHeight]
    dnn_depth_texture: manifold_gpu::GpuTexture, // Rgba8Unorm CPU-upload texture
    // DNN subject mask CPU path
    dnn_has_subject_mask: bool,
    dnn_subject_dirty: bool,
    _dnn_subject_buffer: Vec<f32>, // float[analysisWidth * analysisHeight]
    dnn_subject_history_buffer: Vec<f32>, // float[analysisWidth * analysisHeight]
    dnn_subject_texture: manifold_gpu::GpuTexture, // Rgba8Unorm CPU-upload texture
    // Native flow CPU path
    has_prev_native_frame: bool,
    prev_native_pixel_buffer: Vec<u8>, // byte[analysisWidth * analysisHeight * 4]
    native_flow_buffer: Vec<f32>,      // float[analysisWidth * analysisHeight * 4]
    native_flow_texture: manifold_gpu::GpuTexture, // RGBAFloat → Rgba16Float CPU-upload texture
    native_flow_has_data: bool,
    native_flow_dirty: bool,
    native_flow_ready: bool,
    _cut_score_buffer: Vec<f32>, // float[1]
    latest_cut_score: f32,
    // Timing
    last_native_request_frame: i64,
    last_subject_request_frame: i64,
    last_mesh_update_frame: i64,
    // Request flags
    native_request_wants_flow: bool,
    native_request_wants_depth: bool,
    native_request_wants_subject: bool,
    // GPU readback
    readback: ReadbackRequest,
}

// Uniforms struct for all 15 passes — 16-byte aligned.
// 20 f32 fields = 80 bytes = 5 × vec4. Exactly aligned.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WireUniforms {
    amount: f32,               // _Amount
    grid_density: f32,         // _GridDensity
    line_width: f32,           // _LineWidth
    depth_scale: f32,          // _DepthScale
    temporal_smooth: f32,      // _TemporalSmooth
    persistence: f32,          // _Persistence
    flow_lock_strength: f32,   // _FlowLockStrength
    mesh_regularize: f32,      // _MeshRegularize
    cell_affine_strength: f32, // _CellAffineStrength
    face_warp_strength: f32,   // _FaceWarpStrength
    surface_persistence: f32,  // _SurfacePersistence
    wire_taa: f32,             // _WireTaa
    subject_isolation: f32,    // _SubjectIsolation
    blend_mode: f32,           // _BlendMode
    texel_x: f32,              // _MainTex_TexelSize.x
    texel_y: f32,              // _MainTex_TexelSize.y
    depth_texel_x: f32,        // _DepthTex_TexelSize.x
    depth_texel_y: f32,        // _DepthTex_TexelSize.y
    _pad0: f32,
    _pad1: f32,
}

const _: () = assert!(std::mem::size_of::<WireUniforms>() == 80);

// WireframeDepthFX.cs line 16 — WireframeDepthFX : SimpleBlitEffect, IStatefulEffect
pub struct WireframeDepthFX {
    // 15 compute pipelines — one per shader pass
    compute_pipelines: Vec<manifold_gpu::GpuComputePipeline>,
    sampler: manifold_gpu::GpuSampler,
    // 1×1 dummy texture for texture slots unused by a given pass
    dummy_tex: manifold_gpu::GpuTexture,
    // WireframeDepthFX.cs line 92-93
    owner_states: AHashMap<i64, OwnerState>,
    width: u32,
    height: u32,
    // WireframeDepthFX.cs line 96-101 — DNN backend state
    // Native processing runs on background thread(s) via BackgroundWorker.
    // Parallel mode: 3 independent workers (depth, flow, subject).
    // Monolithic fallback: single worker handling all three tasks.
    // Workers are shared across owners — guarded by is_busy() to prevent
    // drain-to-latest from discarding one owner's request in favour of another.
    // Owner routing is embedded in request/response types.
    workers: Option<WorkerMode>,
    dnn_backend_initialized: bool,
    dnn_backend_available: bool,
    dnn_next_retry_frame: i64,
    warned_missing_dnn: bool,
    dnn_subject_api_available: bool,
    // WireframeDepthFX.cs line 102 — static ompEnvConfigured
    // Handled in FfiDepthEstimator::new() — KMP_DUPLICATE_LIB_OK set there.
}

unsafe impl Send for WireframeDepthFX {}

impl WireframeDepthFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        // --- Native Metal compute pipelines (15 entry points) ---
        let compute_wgsl = include_str!("shaders/fx_wireframe_depth_compute.wgsl");
        let cs_entry_points = [
            "cs_analysis",             // 0
            "cs_wire_mask",            // 1
            "cs_update_history",       // 2
            "cs_composite",            // 3
            "cs_dnn_depth_post",       // 4
            "cs_flow_estimate",        // 5
            "cs_flow_advect_coord",    // 6
            "cs_init_mesh_coord",      // 7
            "cs_mesh_regularize",      // 8
            "cs_mesh_cell_affine",     // 9
            "cs_semantic_mask",        // 10
            "cs_mesh_face_warp",       // 11
            "cs_surface_cache_update", // 12
            "cs_flow_hygiene",         // 13
        ];
        let compute_pipelines: Vec<manifold_gpu::GpuComputePipeline> = cs_entry_points
            .iter()
            .enumerate()
            .map(|(i, ep)| {
                device.create_compute_pipeline(compute_wgsl, ep, &format!("WireframeDepth P{i}"))
            })
            .collect();

        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc::default());

        let dummy_tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: 1,
            height: 1,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba8Unorm,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "WireframeDepth Dummy",
            mip_levels: 1,
        });

        // WireframeDepthFX.cs line 96-101 — try to create native backend
        // Plugin is created on the worker thread (single creation, no probe).
        let workers = Self::try_spawn_workers();
        let dnn_backend_available = workers.is_some();
        let dnn_backend_initialized = workers.is_some();

        Self {
            compute_pipelines,
            sampler,
            dummy_tex,
            owner_states: AHashMap::new(),
            width: 0,
            height: 0,
            workers,
            dnn_backend_initialized,
            dnn_backend_available,
            dnn_next_retry_frame: 0,
            warned_missing_dnn: false,
            dnn_subject_api_available: true,
        }
    }

    // WireframeDepthFX.cs line 259-268 — CreateRenderTexture helper.
    // Uses TexturePool when available (heap sub-allocation, zero kernel calls).
    fn create_rt(
        pool: Option<&manifold_gpu::TexturePool>,
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
        format: manifold_gpu::GpuTextureFormat,
        label: &str,
    ) -> RenderTarget {
        if let Some(pool) = pool {
            RenderTarget::new_pooled(pool, w, h, format, label)
        } else {
            RenderTarget::new(device, w, h, format, label)
        }
    }

    /// Create a transient RenderTarget for per-frame intermediate use.
    /// Release back to pool via `release_transient()` after GPU commands
    /// are encoded. The pool's frame-stamped recycling ensures the texture
    /// won't be reused until `frames_in_flight` frames have passed.
    fn create_transient(
        gpu: &GpuEncoder,
        w: u32,
        h: u32,
        format: manifold_gpu::GpuTextureFormat,
        label: &str,
    ) -> RenderTarget {
        if let Some(pool) = gpu.pool {
            RenderTarget::new_pooled(pool, w, h, format, label)
        } else {
            RenderTarget::new(gpu.device, w, h, format, label)
        }
    }

    /// Release a transient RenderTarget back to the pool for future reuse.
    /// Frame-stamped — won't be recycled until the GPU is done with it.
    fn release_transient(gpu: &GpuEncoder, rt: RenderTarget) {
        if let Some(pool) = gpu.pool {
            rt.release_to_pool(pool);
        }
        // If no pool, rt drops normally (Metal frees the texture).
    }

    /// Dispatch a compute pass via native Metal encoder.
    /// All 15 passes share the same binding layout:
    ///   0=uniforms, 1-12=textures, 13=sampler, 14=output storage texture.
    #[allow(clippy::too_many_arguments)]
    fn encode_pass(
        &self,
        gpu: &mut GpuEncoder,
        pass_idx: usize,
        uniforms: &WireUniforms,
        main_tex: &manifold_gpu::GpuTexture,
        prev_analysis_tex: &manifold_gpu::GpuTexture,
        prev_depth_tex: &manifold_gpu::GpuTexture,
        depth_tex: &manifold_gpu::GpuTexture,
        history_tex: &manifold_gpu::GpuTexture,
        flow_tex: &manifold_gpu::GpuTexture,
        mesh_coord_tex: &manifold_gpu::GpuTexture,
        prev_mesh_coord_tex: &manifold_gpu::GpuTexture,
        semantic_tex: &manifold_gpu::GpuTexture,
        surface_cache_tex: &manifold_gpu::GpuTexture,
        prev_surface_cache_tex: &manifold_gpu::GpuTexture,
        subject_mask_tex: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        w: u32,
        h: u32,
    ) {
        let pipeline = &self.compute_pipelines[pass_idx];
        let uniform_bytes = bytemuck::bytes_of(uniforms);

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: uniform_bytes,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: main_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: prev_analysis_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: prev_depth_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 4,
                    texture: depth_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 5,
                    texture: history_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 6,
                    texture: flow_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 7,
                    texture: mesh_coord_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 8,
                    texture: prev_mesh_coord_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 9,
                    texture: semantic_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 10,
                    texture: surface_cache_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 11,
                    texture: prev_surface_cache_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 12,
                    texture: subject_mask_tex,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 13,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 14,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "WireframeDepth Pass",
        );
    }

    /// Copy texture to texture via native Metal blit.
    fn encode_copy(
        gpu: &mut GpuEncoder,
        src: &manifold_gpu::GpuTexture,
        dst: &manifold_gpu::GpuTexture,
        w: u32,
        h: u32,
    ) {
        gpu.copy_texture_to_texture(src, dst, w, h);
    }

    /// Clear a RenderTarget to black via native Metal.
    fn encode_clear(gpu: &mut GpuEncoder, rt: &RenderTarget) {
        gpu.clear_texture(&rt.texture, 0.0, 0.0, 0.0, 1.0);
    }

    // Create a CPU-upload 2D texture (Rgba8Unorm or Rgba16Float) for DNN outputs.
    fn create_cpu_texture(
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
        format: manifold_gpu::GpuTextureFormat,
        label: &str,
    ) -> manifold_gpu::GpuTexture {
        device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL
                | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
            label,
            mip_levels: 1,
        })
    }

    // WireframeDepthFX.cs line 139-238 — GetOrCreateOwner
    fn get_or_create_owner(
        &mut self,
        gpu: &mut GpuEncoder,
        owner_key: i64,
        wire_scale: f32,
    ) -> &mut OwnerState {
        // WireframeDepthFX.cs line 141-142
        let wire_w = (self.width as f32 * wire_scale).round() as u32;
        let wire_w = wire_w.max(64);
        let wire_h = (self.height as f32 * wire_scale).round() as u32;
        let wire_h = wire_h.max(36);

        // WireframeDepthFX.cs line 144-162: if exists and valid, rebuild wire RT only
        // if scale changed
        if let Some(state) = self.owner_states.get_mut(&owner_key) {
            if state.wire_width != wire_w || state.wire_height != wire_h {
                // Rebuild line history RT only
                state.wire_width = wire_w;
                state.wire_height = wire_h;
                state.line_history_tex = Self::create_rt(
                    gpu.pool,
                    gpu.device,
                    wire_w,
                    wire_h,
                    manifold_gpu::GpuTextureFormat::Rgba8Unorm,
                    &format!("WireframeDepthHistory_{owner_key}"),
                );
                Self::encode_clear(gpu, &state.line_history_tex);
            }
            // Rust borrow checker: re-borrow mutably after the if-chain
            return self.owner_states.get_mut(&owner_key).unwrap();
        }

        // WireframeDepthFX.cs line 164-165: release stale state (handled by drop on
        // overwrite below)

        // WireframeDepthFX.cs line 167-169
        let scale = (MAX_ANALYSIS_DIM as f32 / self.width.max(self.height) as f32).min(1.0);
        let analysis_width = ((self.width as f32 * scale).round() as u32).max(64);
        let analysis_height = ((self.height as f32 * scale).round() as u32).max(36);

        let aw = analysis_width;
        let ah = analysis_height;
        let pixel_count = (aw * ah) as usize;

        let previous_analysis_tex = Self::create_rt(
            gpu.pool,
            gpu.device,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba8Unorm,
            &format!("WireframeDepthPrev_{owner_key}"),
        );
        let depth_tex = Self::create_rt(
            gpu.pool,
            gpu.device,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            &format!("WireframeDepthDepth_{owner_key}"),
        );
        let line_history_tex = Self::create_rt(
            gpu.pool,
            gpu.device,
            wire_w,
            wire_h,
            manifold_gpu::GpuTextureFormat::Rgba8Unorm,
            &format!("WireframeDepthHistory_{owner_key}"),
        );
        let flow_tex = Self::create_rt(
            gpu.pool,
            gpu.device,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            &format!("WireframeDepthFlow_{owner_key}"),
        );
        let mesh_coord_tex = Self::create_rt(
            gpu.pool,
            gpu.device,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            &format!("WireframeDepthMeshCoord_{owner_key}"),
        );
        let semantic_tex = Self::create_rt(
            gpu.pool,
            gpu.device,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            &format!("WireframeDepthSemantic_{owner_key}"),
        );
        let surface_cache_tex = Self::create_rt(
            gpu.pool,
            gpu.device,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            &format!("WireframeDepthSurface_{owner_key}"),
        );
        let dnn_input_tex = Self::create_rt(
            gpu.pool,
            gpu.device,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba8Unorm,
            &format!("WireframeDepthDnnInput_{owner_key}"),
        );

        // WireframeDepthFX.cs line 205-222 — CPU upload textures
        let dnn_depth_texture = Self::create_cpu_texture(
            gpu.device,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba8Unorm,
            &format!("WireframeDepthDnnDepth_{owner_key}"),
        );
        // Unity: RGBAFloat (Rgba32Float), but Rgba32Float is NOT filterable on Metal.
        // textureSample requires filterable; Rgba16Float is the approved Metal fallback.
        // Upload converts f32 → f16 in upload_native_flow_texture().
        let native_flow_texture = Self::create_cpu_texture(
            gpu.device,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            &format!("WireframeDepthNativeFlow_{owner_key}"),
        );
        let dnn_subject_texture = Self::create_cpu_texture(
            gpu.device,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba8Unorm,
            &format!("WireframeDepthDnnSubject_{owner_key}"),
        );

        // WireframeDepthFX.cs line 224-231: clear RTs
        Self::encode_clear(gpu, &previous_analysis_tex);
        Self::encode_clear(gpu, &depth_tex);
        Self::encode_clear(gpu, &line_history_tex);
        Self::encode_clear(gpu, &flow_tex);
        Self::encode_clear(gpu, &semantic_tex);
        Self::encode_clear(gpu, &surface_cache_tex);
        Self::encode_clear(gpu, &dnn_input_tex);

        let mut state = OwnerState {
            analysis_width: aw,
            analysis_height: ah,
            wire_width: wire_w,
            wire_height: wire_h,
            previous_analysis_tex,
            depth_tex,
            line_history_tex,
            flow_tex,
            mesh_coord_tex,
            semantic_tex,
            surface_cache_tex,
            dnn_input_tex,
            dnn_readback_pending: false,
            dnn_has_depth: false,
            dnn_depth_dirty: false,
            _dnn_pixel_buffer: vec![0u8; pixel_count * 4],
            dnn_depth_buffer: vec![0.0f32; pixel_count],
            dnn_depth_texture,
            dnn_has_subject_mask: false,
            dnn_subject_dirty: false,
            _dnn_subject_buffer: vec![0.0f32; pixel_count],
            dnn_subject_history_buffer: vec![0.0f32; pixel_count],
            dnn_subject_texture,
            has_prev_native_frame: false,
            prev_native_pixel_buffer: vec![0u8; pixel_count * 4],
            native_flow_buffer: vec![0.0f32; pixel_count * 4],
            native_flow_texture,
            native_flow_has_data: false,
            native_flow_dirty: false,
            native_flow_ready: false,
            _cut_score_buffer: vec![0.0f32; 1],
            latest_cut_score: 0.0,
            last_native_request_frame: -1024,
            last_subject_request_frame: -1024,
            last_mesh_update_frame: -1024,
            native_request_wants_flow: false,
            native_request_wants_depth: false,
            native_request_wants_subject: false,
            readback: ReadbackRequest::new(),
        };

        // WireframeDepthFX.cs line 231 — InitializeMeshCoord
        self.initialize_mesh_coord_new(gpu, &mut state);

        self.owner_states.insert(owner_key, state);
        self.owner_states.get_mut(&owner_key).unwrap()
    }

    // WireframeDepthFX.cs line 240-257 — InitializeMeshCoord
    // Called during owner creation. Runs PASS_INIT_MESH_COORD then
    // PASS_SURFACE_CACHE_UPDATE.
    fn initialize_mesh_coord_new(&self, gpu: &mut GpuEncoder, state: &mut OwnerState) {
        // WireframeDepthFX.cs line 242: if meshCoordTex == null return
        // (always valid here since we just created it)

        let aw = state.analysis_width;
        let ah = state.analysis_height;

        // Uniforms with zeroed scalars — only texel size matters for init pass
        let uniforms = WireUniforms {
            texel_x: 1.0 / aw as f32,
            texel_y: 1.0 / ah as f32,
            depth_texel_x: 1.0 / aw as f32,
            depth_texel_y: 1.0 / ah as f32,
            surface_persistence: 0.9,
            ..bytemuck::Zeroable::zeroed()
        };

        // PASS_INIT_MESH_COORD: null → meshCoordTex
        // In Unity: Graphics.Blit(null, state.meshCoordTex, material,
        //   PASS_INIT_MESH_COORD)
        // We bind dummy for all textures.
        self.encode_pass(
            gpu,
            PASS_INIT_MESH_COORD,
            &uniforms,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &state.mesh_coord_tex.texture,
            aw,
            ah,
        );
        // PASS_SURFACE_CACHE_UPDATE from fresh mesh coord
        self.encode_pass(
            gpu,
            PASS_SURFACE_CACHE_UPDATE,
            &uniforms,
            &state.mesh_coord_tex.texture,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &state.surface_cache_tex.texture,
            aw,
            ah,
        );
    }

    // WireframeDepthFX.cs line 538-554 — UploadDnnDepthTexture
    fn upload_dnn_depth_texture(gpu: &mut GpuEncoder, state: &mut OwnerState) {
        if !state.dnn_depth_dirty {
            return;
        }
        let count = (state.analysis_width * state.analysis_height) as usize;
        let mut pixels = vec![0u8; count * 4];
        for i in 0..count {
            let v = (state.dnn_depth_buffer[i].clamp(0.0, 1.0) * 255.0) as u8;
            pixels[i * 4] = v;
            pixels[i * 4 + 1] = v;
            pixels[i * 4 + 2] = v;
            pixels[i * 4 + 3] = 255;
        }
        gpu.native_enc.upload_texture(
            &state.dnn_depth_texture,
            state.analysis_width,
            state.analysis_height,
            1,
            &pixels,
        );
        state.dnn_depth_dirty = false;
    }

    // WireframeDepthFX.cs line 556-572 — UploadDnnSubjectTexture
    fn upload_dnn_subject_texture(gpu: &mut GpuEncoder, state: &mut OwnerState) {
        if !state.dnn_subject_dirty {
            return;
        }
        let count = (state.analysis_width * state.analysis_height) as usize;
        let mut pixels = vec![0u8; count * 4];
        for i in 0..count {
            let v = (state.dnn_subject_history_buffer[i].clamp(0.0, 1.0) * 255.0) as u8;
            pixels[i * 4] = v;
            pixels[i * 4 + 1] = v;
            pixels[i * 4 + 2] = v;
            pixels[i * 4 + 3] = 255;
        }
        gpu.native_enc.upload_texture(
            &state.dnn_subject_texture,
            state.analysis_width,
            state.analysis_height,
            1,
            &pixels,
        );
        state.dnn_subject_dirty = false;
    }

    // WireframeDepthFX.cs line 574-594 — UploadNativeFlowTexture
    // nativeFlowPixels is Color (RGBAFloat) → upload as Rgba16Float
    //   (Metal: Rgba32Float not filterable)
    fn upload_native_flow_texture(gpu: &mut GpuEncoder, state: &mut OwnerState) {
        if !state.native_flow_dirty {
            return;
        }
        let count = (state.analysis_width * state.analysis_height) as usize;
        // Convert f32 flow data → f16 for Rgba16Float upload
        let floats = &state.native_flow_buffer[..count * 4];
        let mut f16_bytes: Vec<u8> = Vec::with_capacity(count * 8); // 4 halves × 2 bytes
        for &f in floats {
            f16_bytes.extend_from_slice(&f32_to_f16(f).to_le_bytes());
        }
        gpu.native_enc.upload_texture(
            &state.native_flow_texture,
            state.analysis_width,
            state.analysis_height,
            1,
            &f16_bytes,
        );
        state.native_flow_dirty = false;
    }

    /// Try to spawn 3 parallel workers (depth, flow, subject).
    /// Returns None if the plugin doesn't support specialized creation.
    fn try_spawn_parallel_workers() -> Option<WorkerMode> {
        let depth_worker = BackgroundWorker::try_new(|| {
            use manifold_native::depth_estimator::DepthEstimator;
            let mut estimator =
                manifold_native::ffi::depth_ffi::FfiDepthEstimator::new_depth_only()?;
            Some(move |req: DepthOnlyRequest| -> DepthOnlyResponse {
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
                DepthOnlyResponse {
                    owner_key: req.owner_key,
                    depth_buffer: if ok != 0 { Some(depth) } else { None },
                }
            })
        })?;

        let flow_worker = BackgroundWorker::try_new(|| {
            use manifold_native::depth_estimator::DepthEstimator;
            let mut estimator =
                manifold_native::ffi::depth_ffi::FfiDepthEstimator::new_flow_only()?;
            Some(move |req: FlowOnlyRequest| -> FlowOnlyResponse {
                if !req.has_prev_frame {
                    return FlowOnlyResponse {
                        owner_key: req.owner_key,
                        flow_buffer: None,
                        cut_score: 0.0,
                    };
                }
                let pc = (req.width * req.height) as usize;
                let mut flow = vec![0f32; pc * 4];
                let mut cut = vec![0f32; 1];
                let ok = estimator.compute_flow(
                    &req.prev_pixel_data,
                    &req.pixel_data,
                    req.width,
                    req.height,
                    &mut flow,
                    req.width,
                    req.height,
                    &mut cut,
                );
                if ok != 0 {
                    FlowOnlyResponse {
                        owner_key: req.owner_key,
                        flow_buffer: Some(flow),
                        cut_score: cut[0],
                    }
                } else {
                    FlowOnlyResponse {
                        owner_key: req.owner_key,
                        flow_buffer: None,
                        cut_score: 0.0,
                    }
                }
            })
        })?;

        let subject_worker = BackgroundWorker::try_new(|| {
            use manifold_native::depth_estimator::DepthEstimator;
            let mut estimator =
                manifold_native::ffi::depth_ffi::FfiDepthEstimator::new_subject_only()?;
            Some(move |req: SubjectOnlyRequest| -> SubjectOnlyResponse {
                let pc = (req.width * req.height) as usize;
                let mut mask = vec![0f32; pc];
                let ok = estimator.process_subject_mask(
                    &req.pixel_data,
                    req.width,
                    req.height,
                    &mut mask,
                    req.width,
                    req.height,
                );
                if ok != 0 {
                    const BLEND: f32 = 0.55;
                    let blended: Vec<f32> = if req.has_subject_mask_history {
                        let mut hist = req.subject_history;
                        for i in 0..pc {
                            hist[i] = hist[i] + (mask[i].clamp(0.0, 1.0) - hist[i]) * BLEND;
                        }
                        hist
                    } else {
                        mask.iter().map(|v| v.clamp(0.0, 1.0)).collect()
                    };
                    SubjectOnlyResponse {
                        owner_key: req.owner_key,
                        subject_history_blended: Some(blended),
                        subject_api_failed: false,
                    }
                } else {
                    SubjectOnlyResponse {
                        owner_key: req.owner_key,
                        subject_history_blended: None,
                        subject_api_failed: true,
                    }
                }
            })
        })?;

        Some(WorkerMode::Parallel {
            depth_worker,
            flow_worker,
            subject_worker,
        })
    }

    /// Try to spawn workers: parallel mode first, monolithic fallback.
    fn try_spawn_workers() -> Option<WorkerMode> {
        if let Some(parallel) = Self::try_spawn_parallel_workers() {
            log::info!("[WireframeDepthFX] Spawned 3 parallel native workers");
            return Some(parallel);
        }
        let worker = Self::try_spawn_monolithic_worker()?;
        log::info!(
            "[WireframeDepthFX] Parallel spawn failed; \
             falling back to monolithic worker"
        );
        Some(WorkerMode::Monolithic { worker })
    }

    /// Try to spawn a BackgroundWorker that owns the DepthEstimator (monolithic mode).
    /// Returns None if the native plugin isn't available.
    fn try_spawn_monolithic_worker() -> Option<BackgroundWorker<DepthRequest, DepthResponse>> {
        BackgroundWorker::try_new(|| {
            use manifold_native::depth_estimator::DepthEstimator;
            let mut estimator = manifold_native::ffi::depth_ffi::FfiDepthEstimator::new()?;
            Some(move |req: DepthRequest| -> DepthResponse {
                let w = req.width;
                let h = req.height;
                let pixel_count = (w * h) as usize;

                // Flow
                let (flow_buffer, cut_score) = if req.wants_flow && req.has_prev_frame {
                    let mut flow = vec![0f32; pixel_count * 4];
                    let mut cut = vec![0f32; 1];
                    let ok = estimator.compute_flow(
                        &req.prev_pixel_data,
                        &req.pixel_data,
                        w,
                        h,
                        &mut flow,
                        w,
                        h,
                        &mut cut,
                    );
                    if ok != 0 {
                        (Some(flow), cut[0])
                    } else {
                        (None, 0.0)
                    }
                } else {
                    (None, 0.0)
                };

                // Depth
                let depth_buffer = if req.wants_depth {
                    let mut depth = vec![0f32; pixel_count];
                    let ok = estimator.process(&req.pixel_data, w, h, &mut depth, w, h);
                    if ok != 0 { Some(depth) } else { None }
                } else {
                    None
                };

                // Subject mask + temporal blend
                let (subject_history_blended, subject_api_failed) = if req.wants_subject {
                    let mut mask = vec![0f32; pixel_count];
                    let ok = estimator.process_subject_mask(&req.pixel_data, w, h, &mut mask, w, h);
                    if ok != 0 {
                        // Temporal blend on worker thread (cheap, data is local)
                        const BLEND: f32 = 0.55;
                        let blended: Vec<f32> = if req.has_subject_mask_history {
                            let mut hist = req.subject_history;
                            for i in 0..pixel_count {
                                let current = mask[i].clamp(0.0, 1.0);
                                hist[i] = hist[i] + (current - hist[i]) * BLEND;
                            }
                            hist
                        } else {
                            mask.iter().map(|v| v.clamp(0.0, 1.0)).collect()
                        };
                        (Some(blended), false)
                    } else {
                        // API not available in this plugin build
                        (None, true)
                    }
                } else {
                    (None, false)
                };

                DepthResponse {
                    owner_key: req.owner_key,
                    flow_buffer,
                    cut_score,
                    depth_buffer,
                    subject_history_blended,
                    subject_api_failed,
                }
            })
        })
    }

    // WireframeDepthFX.cs line 497-525 — EnsureDnnBackendAvailable
    // Returns whether backend is ready. If FfiDepthEstimator already loaded in new(),
    // this just returns the cached state. Retry after 300 frames on failure.
    fn ensure_dnn_backend_available(&mut self, frame_count: i64) -> bool {
        if self.dnn_backend_initialized && self.dnn_backend_available {
            return true;
        }
        if self.dnn_backend_initialized
            && !self.dnn_backend_available
            && frame_count < self.dnn_next_retry_frame
        {
            return false;
        }

        // Retry loading the native plugin (created on worker thread, no probe)
        let workers = Self::try_spawn_workers();
        self.dnn_backend_available = workers.is_some();
        self.workers = workers;
        self.dnn_backend_initialized = true;
        if !self.dnn_backend_available {
            self.dnn_next_retry_frame = frame_count + 300;
        }
        self.dnn_backend_available
    }

    // WireframeDepthFX.cs line 455-495 — RequestNativeReadback
    fn request_native_readback(
        &mut self,
        gpu: &mut GpuEncoder,
        source_tex: &manifold_gpu::GpuTexture,
        owner_key: i64,
        subject_isolation: f32,
        frame_count: i64,
    ) {
        let state = match self.owner_states.get_mut(&owner_key) {
            Some(s) => s,
            None => return,
        };

        // WireframeDepthFX.cs line 465-472
        let wants_subject = self.dnn_subject_api_available
            && subject_isolation > 0.02
            && frame_count - state.last_subject_request_frame >= NATIVE_UPDATE_INTERVAL_SUBJECT;

        // WireframeDepthFX.cs line 475-478
        if frame_count - state.last_native_request_frame < NATIVE_UPDATE_INTERVAL_DNN {
            return;
        }

        if !self.ensure_dnn_backend_available(frame_count) {
            return;
        }

        let state = match self.owner_states.get_mut(&owner_key) {
            Some(s) => s,
            None => return,
        };

        if state.dnn_readback_pending {
            return;
        }

        // WireframeDepthFX.cs line 483-494: blit source → dnnInputTex, then readback
        // Copy source → dnn_input_tex via blit (both Rgba8Unorm)
        let copy_aw = state.analysis_width;
        let copy_ah = state.analysis_height;
        Self::encode_copy(
            gpu,
            source_tex,
            &state.dnn_input_tex.texture,
            copy_aw,
            copy_ah,
        );

        state.native_request_wants_depth = true;
        state.native_request_wants_flow = true;
        state.native_request_wants_subject = wants_subject;
        state.last_native_request_frame = frame_count;
        if wants_subject {
            state.last_subject_request_frame = frame_count;
        }

        let aw = state.analysis_width;
        let ah = state.analysis_height;
        // Native shared-memory readback via GpuEncoder.
        state
            .readback
            .submit(gpu, &state.dnn_input_tex.texture, aw, ah);
        state.dnn_readback_pending = true;
    }

    // Apply a completed DepthResponse from the background worker to OwnerState.
    // Replaces the old on_native_readback_complete which ran FFI inline.
    fn apply_depth_response(state: &mut OwnerState, response: &DepthResponse) {
        // Flow
        if let Some(ref flow) = response.flow_buffer {
            let copy_len = flow.len().min(state.native_flow_buffer.len());
            state.native_flow_buffer[..copy_len].copy_from_slice(&flow[..copy_len]);
            state.native_flow_has_data = true;
            state.native_flow_dirty = true;
            state.native_flow_ready = true;
            state.latest_cut_score = response.cut_score;
        } else {
            state.native_flow_has_data = false;
            state.native_flow_ready = false;
            state.latest_cut_score = 0.0;
        }

        // Depth
        if let Some(ref depth) = response.depth_buffer {
            let copy_len = depth.len().min(state.dnn_depth_buffer.len());
            state.dnn_depth_buffer[..copy_len].copy_from_slice(&depth[..copy_len]);
            state.dnn_has_depth = true;
            state.dnn_depth_dirty = true;
        }

        // Subject mask (temporally blended on worker thread)
        if let Some(ref blended) = response.subject_history_blended {
            let copy_len = blended.len().min(state.dnn_subject_history_buffer.len());
            state.dnn_subject_history_buffer[..copy_len].copy_from_slice(&blended[..copy_len]);
            state.dnn_has_subject_mask = true;
            state.dnn_subject_dirty = true;
        }
    }

    // WireframeDepthFX.cs line 420-453 — TryEstimateDepthDnn
    fn try_estimate_depth_dnn(
        &self,
        gpu: &mut GpuEncoder,
        state: &mut OwnerState,
        _temporal_smooth: f32,
        uniforms: &WireUniforms,
    ) -> bool {
        // dnnBackendAvailable checked by caller (ensure_dnn_backend_available)
        if state.dnn_depth_dirty {
            Self::upload_dnn_depth_texture(gpu, state);
        }
        if !state.dnn_has_depth {
            return false;
        }

        let aw = state.analysis_width;
        let ah = state.analysis_height;
        let depth_next = Self::create_transient(
            gpu,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            "WD DnnDepthNext",
        );

        // PASS_DNN_DEPTH_POST: dnnDepthTexture → depthNext
        self.encode_pass(
            gpu,
            PASS_DNN_DEPTH_POST,
            uniforms,
            &state.dnn_depth_texture,
            &self.dummy_tex,
            &state.depth_tex.texture,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &depth_next.texture,
            aw,
            ah,
        );

        // Graphics.Blit(depthNext, state.depthTex)
        Self::encode_copy(gpu, &depth_next.texture, &state.depth_tex.texture, aw, ah);
        Self::release_transient(gpu, depth_next);
        true
    }

    // WireframeDepthFX.cs line 730-892 — UpdateFlowLock
    fn update_flow_lock(
        &self,
        gpu: &mut GpuEncoder,
        analysis_tex: &manifold_gpu::GpuTexture,
        state: &mut OwnerState,
        _temporal_smooth: f32,
        mesh_rate: i32,
        native_flow_enabled: bool,
        face_warp_enabled: bool,
        frame_count: i64,
        uniforms: &WireUniforms,
    ) {
        // WireframeDepthFX.cs line 738-740
        // (null checks — all fields valid if we reached here)
        // WireframeDepthFX.cs line 742-743
        if native_flow_enabled && state.native_flow_dirty {
            Self::upload_native_flow_texture(gpu, state);
        }

        // WireframeDepthFX.cs line 747-748
        let use_native_flow = native_flow_enabled && state.native_flow_has_data;
        // WireframeDepthFX.cs line 750-766 — scene cut hard reset
        if use_native_flow && state.latest_cut_score > 0.28 {
            let aw = state.analysis_width;
            let ah = state.analysis_height;
            // Clear lineHistoryTex, semanticTex
            Self::encode_clear(gpu, &state.line_history_tex);
            Self::encode_clear(gpu, &state.semantic_tex);
            // Re-initialize mesh coord
            let cut_uniforms = WireUniforms {
                surface_persistence: 0.9,
                texel_x: 1.0 / aw as f32,
                texel_y: 1.0 / ah as f32,
                depth_texel_x: 1.0 / aw as f32,
                depth_texel_y: 1.0 / ah as f32,
                ..bytemuck::Zeroable::zeroed()
            };
            self.encode_pass(
                gpu,
                PASS_INIT_MESH_COORD,
                &cut_uniforms,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &state.mesh_coord_tex.texture,
                aw,
                ah,
            );
            self.encode_pass(
                gpu,
                PASS_SURFACE_CACHE_UPDATE,
                &cut_uniforms,
                &state.mesh_coord_tex.texture,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &state.surface_cache_tex.texture,
                aw,
                ah,
            );

            state.dnn_has_subject_mask = false;
            if !state.dnn_subject_history_buffer.is_empty() {
                state.dnn_subject_history_buffer.fill(0.0);
            }
            state.latest_cut_score = 0.0;
            state.native_flow_ready = false;
            state.native_flow_has_data = false;
            state.last_mesh_update_frame = frame_count;
            // Blit analysis → previousAnalysisTex
            Self::encode_copy(
                gpu,
                &state.dnn_input_tex.texture,
                &state.previous_analysis_tex.texture,
                aw,
                ah,
            );
            return;
        }
        // WireframeDepthFX.cs line 770-776 — amortization check
        let run_mesh_pipeline =
            mesh_rate <= 1 || frame_count - state.last_mesh_update_frame >= mesh_rate as i64;
        if !run_mesh_pipeline {
            return;
        }
        state.last_mesh_update_frame = frame_count;

        let aw = state.analysis_width;
        let ah = state.analysis_height;
        // WireframeDepthFX.cs line 779-789 — choose flow source
        let flow_input_tex: &manifold_gpu::GpuTexture = if use_native_flow {
            &state.native_flow_texture
        } else {
            // PASS_FLOW_ESTIMATE: analysis → flowTex
            self.encode_pass(
                gpu,
                PASS_FLOW_ESTIMATE,
                uniforms,
                analysis_tex,
                &state.previous_analysis_tex.texture,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &state.flow_tex.texture,
                aw,
                ah,
            );
            &state.flow_tex.texture
        };

        // WireframeDepthFX.cs line 792-826 — flowFiltered, temp RTs
        let flow_filtered = Self::create_transient(
            gpu,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            "WD FlowFiltered",
        );
        // PASS_FLOW_HYGIENE: flowInput → flowFiltered
        self.encode_pass(
            gpu,
            PASS_FLOW_HYGIENE,
            uniforms,
            flow_input_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &flow_filtered.texture,
            aw,
            ah,
        );
        let flow_stable_tex = &flow_filtered.texture;

        // WireframeDepthFX.cs line 808-826: semantic mask
        // PASS_SEMANTIC_MASK: analysis → semanticTex
        self.encode_pass(
            gpu,
            PASS_SEMANTIC_MASK,
            uniforms,
            analysis_tex,
            &state.previous_analysis_tex.texture,
            &self.dummy_tex,
            &state.depth_tex.texture,
            &self.dummy_tex,
            flow_stable_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &state.semantic_tex.texture,
            aw,
            ah,
        );

        // WireframeDepthFX.cs line 811-826: temp coord RTs
        let coord_next = Self::create_transient(
            gpu,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            "WD CoordNext",
        );
        let coord_affine = Self::create_transient(
            gpu,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            "WD CoordAffine",
        );
        let coord_regularized = Self::create_transient(
            gpu,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            "WD CoordReg",
        );
        let surface_next = Self::create_transient(
            gpu,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            "WD SurfaceNext",
        );

        // WireframeDepthFX.cs line 829-835: PASS_FLOW_ADVECT_COORD
        self.encode_pass(
            gpu,
            PASS_FLOW_ADVECT_COORD,
            uniforms,
            analysis_tex,
            &state.previous_analysis_tex.texture,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            flow_stable_tex,
            &self.dummy_tex,
            &state.mesh_coord_tex.texture,
            &state.semantic_tex.texture,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &coord_next.texture,
            aw,
            ah,
        );

        // WireframeDepthFX.cs line 837-841: PASS_MESH_CELL_AFFINE
        self.encode_pass(
            gpu,
            PASS_MESH_CELL_AFFINE,
            uniforms,
            &coord_next.texture,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            flow_stable_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &coord_affine.texture,
            aw,
            ah,
        );

        // WireframeDepthFX.cs line 843-862: optional face warp pass
        let pre_regularize_tex: &manifold_gpu::GpuTexture;
        let coord_face_opt: Option<RenderTarget>;
        if face_warp_enabled {
            let coord_face = Self::create_transient(
                gpu,
                aw,
                ah,
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                "WD CoordFace",
            );
            let edge_follow_mask_tex = if state.dnn_has_subject_mask {
                &state.dnn_subject_texture
            } else {
                &state.semantic_tex.texture
            };
            self.encode_pass(
                gpu,
                PASS_MESH_FACE_WARP,
                uniforms,
                &coord_affine.texture,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                flow_stable_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                edge_follow_mask_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &coord_face.texture,
                aw,
                ah,
            );
            coord_face_opt = Some(coord_face);
            pre_regularize_tex = &coord_face_opt.as_ref().unwrap().texture;
        } else {
            coord_face_opt = None;
            pre_regularize_tex = &coord_affine.texture;
        }

        // WireframeDepthFX.cs line 863-871: PASS_MESH_REGULARIZE
        self.encode_pass(
            gpu,
            PASS_MESH_REGULARIZE,
            uniforms,
            pre_regularize_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            flow_stable_tex,
            &self.dummy_tex,
            &state.mesh_coord_tex.texture,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &coord_regularized.texture,
            aw,
            ah,
        );
        Self::encode_copy(
            gpu,
            &coord_regularized.texture,
            &state.mesh_coord_tex.texture,
            aw,
            ah,
        );

        // WireframeDepthFX.cs line 873-879: PASS_SURFACE_CACHE_UPDATE
        self.encode_pass(
            gpu,
            PASS_SURFACE_CACHE_UPDATE,
            uniforms,
            &state.mesh_coord_tex.texture,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            flow_stable_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &state.surface_cache_tex.texture,
            &self.dummy_tex,
            &surface_next.texture,
            aw,
            ah,
        );
        Self::encode_copy(
            gpu,
            &surface_next.texture,
            &state.surface_cache_tex.texture,
            aw,
            ah,
        );
        // Release transient textures back to pool. Frame-stamped recycling
        // ensures they won't be reused until the GPU is done with them.
        if let Some(cf) = coord_face_opt {
            Self::release_transient(gpu, cf);
        }
        Self::release_transient(gpu, flow_filtered);
        Self::release_transient(gpu, coord_next);
        Self::release_transient(gpu, coord_affine);
        Self::release_transient(gpu, coord_regularized);
        Self::release_transient(gpu, surface_next);
    }
}

impl PostProcessEffect for WireframeDepthFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::WIREFRAME_DEPTH
    }

    // WireframeDepthFX.cs line 279-361 — Apply
    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        // WireframeDepthFX.cs line 281-282
        let amount = fx.param_values.first().map(|p| p.value).unwrap_or(0.0);
        if amount <= 0.0 {
            return;
        }

        // Read params — new 12-param layout (see effect_definition_registry.rs)
        let wire_scale = fx
            .param_values
            .get(7)
            .map(|p| p.value)
            .unwrap_or(1.0)
            .clamp(0.5, 1.0);
        let mesh_rate = fx
            .param_values
            .get(8)
            .map(|p| p.value)
            .unwrap_or(1.0)
            .round() as i32;
        let mesh_rate = mesh_rate.clamp(1, 4);
        let native_flow_enabled = fx
            .param_values
            .get(9)
            .map(|p| p.value)
            .unwrap_or(0.0)
            .round() as i32
            > 0;
        let flow_lock_enabled = fx
            .param_values
            .get(10)
            .map(|p| p.value)
            .unwrap_or(0.0)
            .round() as i32
            > 0;
        let face_warp_enabled = fx.param_values.get(11).map(|p| p.value).unwrap_or(0.0) > 0.01;

        // GetOrCreateOwner needs encoder; owner_states borrow released before later
        // use. We store the owner_key to look up the state again after this call.
        let owner_key = ctx.owner_key;
        self.get_or_create_owner(gpu, owner_key, wire_scale);

        // Guard: if frame_count jumped backwards (export restart, seek), reset
        // all frame-throttle counters so readback/mesh updates fire immediately.
        if let Some(state) = self.owner_states.get_mut(&owner_key)
            && ctx.frame_count < state.last_native_request_frame
        {
            state.last_native_request_frame = -1024;
            state.last_subject_request_frame = -1024;
            state.last_mesh_update_frame = -1024;
        }

        // Read remaining params — new 12-param layout
        let density = fx.param_values.get(1).map(|p| p.value).unwrap_or(96.0);
        let line_width = fx.param_values.get(2).map(|p| p.value).unwrap_or(1.2);
        let depth_scale = fx.param_values.get(3).map(|p| p.value).unwrap_or(1.0);
        let temporal_smooth = fx.param_values.get(4).map(|p| p.value).unwrap_or(0.8);
        let persistence = 0.82; // hardcoded default (Persist param removed from UI)
        let subject_isolation = fx
            .param_values
            .get(5)
            .map(|p| p.value)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let blend_mode = fx
            .param_values
            .get(6)
            .map(|p| p.value)
            .unwrap_or(0.0)
            .clamp(0.0, 6.0);
        // ── Poll background worker(s) for completed native results ──
        match &mut self.workers {
            Some(WorkerMode::Parallel {
                depth_worker,
                flow_worker,
                subject_worker,
            }) => {
                if let Some(resp) = depth_worker.try_recv()
                    && let Some(state) = self.owner_states.get_mut(&resp.owner_key)
                    && let Some(ref depth) = resp.depth_buffer
                {
                    let copy_len = depth.len().min(state.dnn_depth_buffer.len());
                    state.dnn_depth_buffer[..copy_len].copy_from_slice(&depth[..copy_len]);
                    state.dnn_has_depth = true;
                    state.dnn_depth_dirty = true;
                }
                if let Some(resp) = flow_worker.try_recv()
                    && let Some(state) = self.owner_states.get_mut(&resp.owner_key)
                {
                    if let Some(ref flow) = resp.flow_buffer {
                        let copy_len = flow.len().min(state.native_flow_buffer.len());
                        state.native_flow_buffer[..copy_len].copy_from_slice(&flow[..copy_len]);
                        state.native_flow_has_data = true;
                        state.native_flow_dirty = true;
                        state.native_flow_ready = true;
                        state.latest_cut_score = resp.cut_score;
                    } else {
                        state.native_flow_has_data = false;
                        state.native_flow_ready = false;
                        state.latest_cut_score = 0.0;
                    }
                }
                if let Some(resp) = subject_worker.try_recv() {
                    if resp.subject_api_failed {
                        self.dnn_subject_api_available = false;
                    }
                    if let Some(state) = self.owner_states.get_mut(&resp.owner_key)
                        && let Some(ref blended) = resp.subject_history_blended
                    {
                        let copy_len = blended.len().min(state.dnn_subject_history_buffer.len());
                        state.dnn_subject_history_buffer[..copy_len]
                            .copy_from_slice(&blended[..copy_len]);
                        state.dnn_has_subject_mask = true;
                        state.dnn_subject_dirty = true;
                    }
                }
            }
            Some(WorkerMode::Monolithic { worker }) => {
                if let Some(response) = worker.try_recv() {
                    if let Some(state) = self.owner_states.get_mut(&response.owner_key) {
                        Self::apply_depth_response(state, &response);
                    }
                    if response.subject_api_failed {
                        self.dnn_subject_api_available = false;
                    }
                }
            }
            None => {}
        }

        // ── Poll GPU readback → submit to background worker(s) ──
        // Native shared-memory readback via GpuEncoder.
        let readback_pixels = if let Some(state) = self.owner_states.get_mut(&owner_key)
            && state.dnn_readback_pending
        {
            state.readback.try_read()
        } else {
            None
        };
        if let Some(pixels) = readback_pixels
            && let Some(state) = self.owner_states.get_mut(&owner_key)
        {
            state.dnn_readback_pending = false;
            let aw = state.analysis_width as i32;
            let ah = state.analysis_height as i32;

            match &mut self.workers {
                Some(WorkerMode::Parallel {
                    depth_worker,
                    flow_worker,
                    subject_worker,
                }) => {
                    // Only submit to each worker if idle — prevents
                    // overwriting another owner's in-flight request.
                    if state.native_request_wants_depth && !depth_worker.is_busy() {
                        depth_worker.submit(DepthOnlyRequest {
                            owner_key,
                            pixel_data: pixels.clone(),
                            width: aw,
                            height: ah,
                        });
                    }
                    if state.native_request_wants_flow && !flow_worker.is_busy() {
                        flow_worker.submit(FlowOnlyRequest {
                            owner_key,
                            pixel_data: pixels.clone(),
                            prev_pixel_data: state.prev_native_pixel_buffer.clone(),
                            has_prev_frame: state.has_prev_native_frame,
                            width: aw,
                            height: ah,
                        });
                    }
                    if state.native_request_wants_subject && !subject_worker.is_busy() {
                        subject_worker.submit(SubjectOnlyRequest {
                            owner_key,
                            pixel_data: pixels.clone(),
                            width: aw,
                            height: ah,
                            has_subject_mask_history: state.dnn_has_subject_mask,
                            subject_history: state.dnn_subject_history_buffer.clone(),
                        });
                    }
                }
                Some(WorkerMode::Monolithic { worker }) => {
                    if !worker.is_busy() {
                        let req = DepthRequest {
                            owner_key,
                            pixel_data: pixels.clone(),
                            prev_pixel_data: state.prev_native_pixel_buffer.clone(),
                            has_prev_frame: state.has_prev_native_frame,
                            width: aw,
                            height: ah,
                            wants_flow: state.native_request_wants_flow,
                            wants_depth: state.native_request_wants_depth,
                            wants_subject: state.native_request_wants_subject,
                            has_subject_mask_history: state.dnn_has_subject_mask,
                            subject_history: state.dnn_subject_history_buffer.clone(),
                        };
                        worker.submit(req);
                    }
                }
                None => {}
            }

            // Copy current → prev immediately (at submit time, not completion).
            let copy_len = pixels.len().min(state.prev_native_pixel_buffer.len());
            state.prev_native_pixel_buffer[..copy_len].copy_from_slice(&pixels[..copy_len]);
            state.has_prev_native_frame = true;
        }
        // Check DNN backend for this frame
        let dnn_available = self.ensure_dnn_backend_available(ctx.frame_count);

        let state = self.owner_states.get(&owner_key).unwrap();
        let aw = state.analysis_width;
        let ah = state.analysis_height;
        let ww = state.wire_width;
        let wh = state.wire_height;

        // Compute derived uniform values.
        // WireframeDepthFX.cs line 829: flowLockStrength =
        //   Lerp(0.76, 0.985, Clamp01(temporalSmooth))
        let ts01 = temporal_smooth.clamp(0.0, 1.0);
        let flow_lock_strength = 0.76 + (0.985 - 0.76) * ts01;
        // WireframeDepthFX.cs line 838: cellAffine = Lerp(0.40, 0.88, ...)
        let cell_affine = 0.40 + (0.88 - 0.40) * ts01;
        // EdgeFollow (param 11) scales the face warp strength.
        // At 1.0 = original behavior.
        let edge_follow = fx
            .param_values
            .get(11)
            .map(|p| p.value)
            .unwrap_or(0.5)
            .clamp(0.0, 1.0);
        let face_warp_strength = (0.25 + (0.90 - 0.25) * ts01) * edge_follow;
        // WireframeDepthFX.cs line 864: regularize = Lerp(0.40, 0.74, ...)
        let mesh_regularize = 0.40 + (0.74 - 0.40) * ts01;
        // WireframeDepthFX.cs line 874: surfacePersist =
        //   Lerp(0.80, 0.985, ...)
        let surface_persistence = 0.80 + (0.985 - 0.80) * ts01;
        // WireframeDepthFX.cs line 334: wireTaa =
        //   Lerp(0.48, 0.92, Clamp01(temporalSmooth))
        let wire_taa = 0.48 + (0.92 - 0.48) * ts01;

        // Build analysis-resolution uniforms
        let uniforms_analysis = WireUniforms {
            amount,
            grid_density: density,
            line_width,
            depth_scale,
            temporal_smooth,
            persistence,
            flow_lock_strength,
            mesh_regularize,
            cell_affine_strength: cell_affine,
            face_warp_strength,
            surface_persistence,
            wire_taa,
            subject_isolation,
            blend_mode,
            texel_x: 1.0 / aw as f32,
            texel_y: 1.0 / ah as f32,
            depth_texel_x: 1.0 / aw as f32,
            depth_texel_y: 1.0 / ah as f32,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        // Wire-resolution uniforms (for pass 3/4)
        let uniforms_wire = WireUniforms {
            texel_x: 1.0 / ww as f32,
            texel_y: 1.0 / wh as f32,
            depth_texel_x: 1.0 / aw as f32,
            depth_texel_y: 1.0 / ah as f32,
            ..uniforms_analysis
        };

        // Source-resolution uniforms (for pass 4 composite)
        let uniforms_source = WireUniforms {
            texel_x: 1.0 / self.width as f32,
            texel_y: 1.0 / self.height as f32,
            ..uniforms_analysis
        };

        // --- EstimateDepth ---
        // WireframeDepthFX.cs line 363-418

        // PASS_ANALYSIS: source → analysis (temp RT at analysis resolution)
        let analysis_rt = Self::create_transient(
            gpu,
            aw,
            ah,
            manifold_gpu::GpuTextureFormat::Rgba8Unorm,
            "WD Analysis",
        );
        self.encode_pass(
            gpu,
            PASS_ANALYSIS,
            &uniforms_analysis,
            source,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &self.dummy_tex,
            &analysis_rt.texture,
            aw,
            ah,
        );

        // WireframeDepthFX.cs line 388-394 — request native readback
        if native_flow_enabled && flow_lock_enabled {
            let mesh_update_due = {
                let state = self.owner_states.get(&owner_key).unwrap();
                mesh_rate <= 1 || ctx.frame_count - state.last_mesh_update_frame >= mesh_rate as i64
            };
            if mesh_update_due {
                // Copy analysis_rt → dnn_input_tex happens inside
                // request_native_readback via encoder copy
                self.request_native_readback(
                    gpu,
                    &analysis_rt.texture,
                    owner_key,
                    subject_isolation,
                    ctx.frame_count,
                );
            }
        }
        // WireframeDepthFX.cs line 396-407 — depth estimation (DNN only)
        // Temporarily remove state to avoid borrow conflict
        // (self.method + self.owner_states)
        if dnn_available {
            let mut state = self.owner_states.remove(&owner_key).unwrap();
            self.try_estimate_depth_dnn(gpu, &mut state, temporal_smooth, &uniforms_analysis);
            self.owner_states.insert(owner_key, state);
        } else if !self.warned_missing_dnn {
            log::warn!(
                "[WireframeDepthFX] DNN depth path requested, but no \
                 backend is configured. Effect will render without depth."
            );
            self.warned_missing_dnn = true;
        }
        // WireframeDepthFX.cs line 409-412 — UpdateFlowLock or blit analysis →
        //   previousAnalysisTex
        if flow_lock_enabled {
            let mut state = self.owner_states.remove(&owner_key).unwrap();
            self.update_flow_lock(
                gpu,
                &analysis_rt.texture,
                &mut state,
                temporal_smooth,
                mesh_rate,
                native_flow_enabled,
                face_warp_enabled,
                ctx.frame_count,
                &uniforms_analysis,
            );
            self.owner_states.insert(owner_key, state);
        }
        // Always copy analysis → previousAnalysisTex
        // (WireframeDepthFX.cs line 412 / 891)
        {
            let state = self.owner_states.get(&owner_key).unwrap();
            Self::encode_copy(
                gpu,
                &analysis_rt.texture,
                &state.previous_analysis_tex.texture,
                aw,
                ah,
            );
        }

        // --- Upload DNN subject texture if dirty ---
        // WireframeDepthFX.cs line 311-312
        {
            let state = self.owner_states.get_mut(&owner_key).unwrap();
            if state.dnn_subject_dirty {
                Self::upload_dnn_subject_texture(gpu, state);
            }
        }

        // --- Wire mask pass (Pass 2) ---
        // WireframeDepthFX.cs line 305-328
        let line_mask = Self::create_transient(
            gpu,
            ww,
            wh,
            manifold_gpu::GpuTextureFormat::Rgba8Unorm,
            "WD LineMask",
        );
        {
            let state = self.owner_states.get(&owner_key).unwrap();
            let subject_mask_tex = if state.dnn_has_subject_mask {
                &state.dnn_subject_texture
            } else {
                &self.dummy_tex
            };
            self.encode_pass(
                gpu,
                PASS_WIREFRAME_MASK,
                &uniforms_wire,
                source,
                &self.dummy_tex,
                &self.dummy_tex,
                &state.depth_tex.texture,
                &self.dummy_tex,
                &self.dummy_tex,
                &state.mesh_coord_tex.texture,
                &self.dummy_tex,
                &state.semantic_tex.texture,
                &state.surface_cache_tex.texture,
                &self.dummy_tex,
                subject_mask_tex,
                &line_mask.texture,
                ww,
                wh,
            );
        }

        // --- Update history pass (Pass 3) + copy → lineHistoryTex ---
        // WireframeDepthFX.cs line 330-347
        let history_next = Self::create_transient(
            gpu,
            ww,
            wh,
            manifold_gpu::GpuTextureFormat::Rgba8Unorm,
            "WD HistoryNext",
        );
        {
            let state = self.owner_states.get(&owner_key).unwrap();
            self.encode_pass(
                gpu,
                PASS_UPDATE_HISTORY,
                &uniforms_wire,
                &line_mask.texture,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &state.line_history_tex.texture,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &state.surface_cache_tex.texture,
                &self.dummy_tex,
                &self.dummy_tex,
                &history_next.texture,
                ww,
                wh,
            );
            Self::encode_copy(
                gpu,
                &history_next.texture,
                &state.line_history_tex.texture,
                ww,
                wh,
            );
        }

        // --- Composite pass (Pass 4) → target ---
        // WireframeDepthFX.cs line 349-355
        {
            let state = self.owner_states.get(&owner_key).unwrap();
            self.encode_pass(
                gpu,
                PASS_COMPOSITE,
                &uniforms_source,
                source,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &state.line_history_tex.texture,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                &self.dummy_tex,
                target,
                self.width,
                self.height,
            );
        }

        // Release transient textures back to pool for frame-stamped recycling.
        Self::release_transient(gpu, analysis_rt);
        Self::release_transient(gpu, line_mask);
        Self::release_transient(gpu, history_next);
    }

    fn flush_background_work(&mut self) {
        match &mut self.workers {
            Some(WorkerMode::Parallel {
                depth_worker,
                flow_worker,
                subject_worker,
            }) => {
                if let Some(resp) = depth_worker.recv_blocking()
                    && let Some(state) = self.owner_states.get_mut(&resp.owner_key)
                    && let Some(ref depth) = resp.depth_buffer
                {
                    let n = depth.len().min(state.dnn_depth_buffer.len());
                    state.dnn_depth_buffer[..n].copy_from_slice(&depth[..n]);
                    state.dnn_has_depth = true;
                    state.dnn_depth_dirty = true;
                }
                if let Some(resp) = flow_worker.recv_blocking()
                    && let Some(state) = self.owner_states.get_mut(&resp.owner_key)
                {
                    if let Some(ref flow) = resp.flow_buffer {
                        let n = flow.len().min(state.native_flow_buffer.len());
                        state.native_flow_buffer[..n].copy_from_slice(&flow[..n]);
                        state.native_flow_has_data = true;
                        state.native_flow_dirty = true;
                        state.native_flow_ready = true;
                        state.latest_cut_score = resp.cut_score;
                    } else {
                        state.native_flow_has_data = false;
                        state.native_flow_ready = false;
                        state.latest_cut_score = 0.0;
                    }
                }
                if let Some(resp) = subject_worker.recv_blocking() {
                    if resp.subject_api_failed {
                        self.dnn_subject_api_available = false;
                    }
                    if let Some(state) = self.owner_states.get_mut(&resp.owner_key)
                        && let Some(ref blended) = resp.subject_history_blended
                    {
                        let n = blended.len().min(state.dnn_subject_history_buffer.len());
                        state.dnn_subject_history_buffer[..n].copy_from_slice(&blended[..n]);
                        state.dnn_has_subject_mask = true;
                        state.dnn_subject_dirty = true;
                    }
                }
            }
            Some(WorkerMode::Monolithic { worker }) => {
                if let Some(response) = worker.recv_blocking() {
                    if let Some(state) = self.owner_states.get_mut(&response.owner_key) {
                        Self::apply_depth_response(state, &response);
                    }
                    if response.subject_api_failed {
                        self.dnn_subject_api_available = false;
                    }
                }
            }
            None => {}
        }
    }

    // WireframeDepthFX.cs line 915-919 — ClearState (all owners)
    fn clear_state(&mut self) {
        // Drops per-owner state entries entirely so a project load
        // doesn't retain previous-project Vec<f32> buffers
        // (`dnn_depth_buffer`, `dnn_subject_history_buffer`,
        // `native_flow_buffer`) in the owner_states map. Next
        // apply() lazy-creates fresh entries. Equivalent to legacy
        // `CleanupAllOwners` — the "reset flags in place" pattern
        // (E-2) was leaking per-owner allocations across project
        // loads.
        self.owner_states.clear();
        self.warned_missing_dnn = false;
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        // WireframeDepthFX.cs line 133-137 — InitializeState
        self.width = width;
        self.height = height;
        // Per-owner textures are rebuilt lazily in GetOrCreateOwner.
        self.owner_states.clear();
    }

}

/// Convert f32 to IEEE 754 half-precision (f16) stored as u16.
/// Used for Rgba16Float CPU uploads where Unity uses Rgba32Float.
fn f32_to_f16(val: f32) -> u16 {
    let bits = val.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xFF) as i32;
    let frac = bits & 0x007F_FFFF;

    if exp == 255 {
        // Inf / NaN
        sign | 0x7C00 | if frac != 0 { 0x0200 } else { 0 }
    } else if exp > 142 {
        // Overflow → Inf
        sign | 0x7C00
    } else if exp > 112 {
        // Normal range
        let e = (exp - 112) as u16;
        sign | (e << 10) | ((frac >> 13) as u16)
    } else if exp > 101 {
        // Subnormal
        let shift = (113 - exp) as u32;
        let f = (frac | 0x0080_0000) >> (shift + 13);
        sign | f as u16
    } else {
        // Underflow → zero
        sign
    }
}
