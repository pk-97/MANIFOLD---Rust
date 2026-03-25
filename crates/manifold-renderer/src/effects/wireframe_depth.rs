// Mechanical port of WireframeDepthFX.cs + WireframeDepthEffect.shader.
// Unity source: Assets/Scripts/Compositing/Effects/WireframeDepthFX.cs (1094 lines)
//              Assets/Shaders/WireframeDepthEffect.shader (15 passes)
//              Assets/Scripts/Compositing/Effects/DepthEstimatorNative.cs
//
// Same logic, same variables, same constants, same edge cases.
// AsyncGPUReadback → poll-based ReadbackRequest (submit + try_read).
// Time.frameCount → EffectContext.frame_count.
// Graphics.Blit → render pass with bind group per call.

use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use wgpu::util::DeviceExt;
use crate::background_worker::BackgroundWorker;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::gpu_encoder::GpuEncoder;
use crate::gpu_readback::ReadbackRequest;
use crate::render_target::RenderTarget;

// Request/response types for the background depth estimation worker.
struct DepthRequest {
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
    flow_buffer: Option<Vec<f32>>,
    cut_score: f32,
    depth_buffer: Option<Vec<f32>>,
    subject_history_blended: Option<Vec<f32>>,
    subject_api_failed: bool,
}

// Per-task request/response types for parallel worker mode.
struct DepthOnlyRequest {
    pixel_data: Vec<u8>,
    width: i32,
    height: i32,
}
struct DepthOnlyResponse {
    depth_buffer: Option<Vec<f32>>,
}

struct FlowOnlyRequest {
    pixel_data: Vec<u8>,
    prev_pixel_data: Vec<u8>,
    has_prev_frame: bool,
    width: i32,
    height: i32,
}
struct FlowOnlyResponse {
    flow_buffer: Option<Vec<f32>>,
    cut_score: f32,
}

struct SubjectOnlyRequest {
    pixel_data: Vec<u8>,
    width: i32,
    height: i32,
    has_subject_mask_history: bool,
    subject_history: Vec<f32>,
}
struct SubjectOnlyResponse {
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
const PASS_ANALYSIS: usize           = 0;
const PASS_HEURISTIC_DEPTH: usize    = 1;
const PASS_WIREFRAME_MASK: usize     = 2;
const PASS_UPDATE_HISTORY: usize     = 3;
const PASS_COMPOSITE: usize          = 4;
const PASS_DNN_DEPTH_POST: usize     = 5;
const PASS_FLOW_ESTIMATE: usize      = 6;
const PASS_FLOW_ADVECT_COORD: usize  = 7;
const PASS_INIT_MESH_COORD: usize    = 8;
const PASS_MESH_REGULARIZE: usize    = 9;
const PASS_MESH_CELL_AFFINE: usize   = 10;
const PASS_SEMANTIC_MASK: usize      = 11;
const PASS_MESH_FACE_WARP: usize     = 12;
const PASS_SURFACE_CACHE_UPDATE: usize = 13;
const PASS_FLOW_HYGIENE: usize       = 14;

// WireframeDepthFX.cs line 36-39
const MAX_ANALYSIS_DIM: u32               = 360;
const NATIVE_UPDATE_INTERVAL_DNN: i64     = 2;
const NATIVE_UPDATE_INTERVAL_HEURISTIC: i64 = 4;
const NATIVE_UPDATE_INTERVAL_SUBJECT: i64  = 4;

// WireframeDepthFX.cs line 41-45
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum DepthSourceMode {
    Heuristic = 0,
    Dnn       = 1,
}

// WireframeDepthFX.cs line 47-90 — OwnerState
// ARGB32  → Rgba8Unorm
// ARGBHalf → Rgba16Float
// RGBAFloat (nativeFlowTexture) → Rgba16Float (Metal: Rgba32Float not filterable; see KNOWN_DIVERGENCES)
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
    dnn_input_tex: RenderTarget,         // ARGB32 → Rgba8Unorm, COPY_SRC for readback
    // DNN depth CPU path
    dnn_readback_pending: bool,
    dnn_has_depth: bool,
    dnn_depth_dirty: bool,
    _dnn_pixel_buffer: Vec<u8>,          // byte[analysisWidth * analysisHeight * 4]
    dnn_depth_buffer: Vec<f32>,          // float[analysisWidth * analysisHeight]
    dnn_depth_texture: wgpu::Texture,    // Rgba8Unorm CPU-upload texture
    dnn_depth_texture_view: wgpu::TextureView,
    // DNN subject mask CPU path
    dnn_has_subject_mask: bool,
    dnn_subject_dirty: bool,
    _dnn_subject_buffer: Vec<f32>,       // float[analysisWidth * analysisHeight]
    dnn_subject_history_buffer: Vec<f32>,// float[analysisWidth * analysisHeight]
    dnn_subject_texture: wgpu::Texture,  // Rgba8Unorm CPU-upload texture
    dnn_subject_texture_view: wgpu::TextureView,
    // Native flow CPU path
    has_prev_native_frame: bool,
    prev_native_pixel_buffer: Vec<u8>,   // byte[analysisWidth * analysisHeight * 4]
    native_flow_buffer: Vec<f32>,        // float[analysisWidth * analysisHeight * 4]
    native_flow_texture: wgpu::Texture,  // RGBAFloat → Rgba16Float CPU-upload texture (Metal: Rgba32Float not filterable)
    native_flow_texture_view: wgpu::TextureView,
    native_flow_has_data: bool,
    native_flow_dirty: bool,
    native_flow_ready: bool,
    _cut_score_buffer: Vec<f32>,         // float[1]
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
    amount: f32,             // _Amount
    grid_density: f32,       // _GridDensity
    line_width: f32,         // _LineWidth
    depth_scale: f32,        // _DepthScale
    temporal_smooth: f32,    // _TemporalSmooth
    persistence: f32,        // _Persistence
    flow_lock_strength: f32, // _FlowLockStrength
    mesh_regularize: f32,    // _MeshRegularize
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

/// BGL entries for the wireframe-depth render pipelines (shared between wgpu and hal).
/// All 15 passes share a single BGL: binding 0 = uniforms, 1–12 = textures, 13 = sampler.
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
const WIREFRAME_DEPTH_BGL_ENTRIES: [wgpu::BindGroupLayoutEntry; 14] = [
    // 0: uniforms
    wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    },
    // 1–12: texture_2d<f32> (filterable float)
    wgpu::BindGroupLayoutEntry {
        binding: 1,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 2,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 3,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 4,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 5,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 6,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 7,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 8,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 9,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 10,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 11,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 12,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    // 13: sampler
    wgpu::BindGroupLayoutEntry {
        binding: 13,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    },
];

/// BGL entries for the wireframe-depth **compute** pipelines (hal path).
/// Same as render BGL (bindings 0–13) plus binding 14: storage texture (write-only).
/// Visibility is COMPUTE instead of VERTEX_FRAGMENT/FRAGMENT.
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
const WIREFRAME_DEPTH_COMPUTE_BGL_ENTRIES: [wgpu::BindGroupLayoutEntry; 15] = [
    // 0: uniforms
    wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    },
    // 1–12: texture_2d<f32> (filterable float)
    wgpu::BindGroupLayoutEntry {
        binding: 1,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 2,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 3,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 4,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 5,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 6,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 7,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 8,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 9,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 10,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 11,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 12,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    // 13: sampler
    wgpu::BindGroupLayoutEntry {
        binding: 13,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    },
    // 14: output storage texture (write-only, rgba16float)
    wgpu::BindGroupLayoutEntry {
        binding: 14,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::Rgba16Float,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    },
];

// WireframeDepthFX.cs line 16 — WireframeDepthFX : SimpleBlitEffect, IStatefulEffect
pub struct WireframeDepthFX {
    // 15 render pipelines — one per shader pass
    pipelines: [wgpu::RenderPipeline; 15],
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    #[allow(dead_code)] // Keeps hal ring buffer alive; wgpu path creates per-call UBOs
    uniform_buffer: wgpu::Buffer,
    // 1×1 dummy texture for texture slots unused by a given pass
    _dummy_tex: wgpu::Texture,
    dummy_view: wgpu::TextureView,
    // WireframeDepthFX.cs line 92-93
    owner_states: AHashMap<i64, OwnerState>,
    width: u32,
    height: u32,
    // WireframeDepthFX.cs line 96-101 — DNN backend state
    // Native processing runs on background thread(s) via BackgroundWorker.
    // Parallel mode: 3 independent workers (depth, flow, subject).
    // Monolithic fallback: single worker handling all three tasks.
    workers: Option<WorkerMode>,
    // Track which owner submitted in-flight worker requests.
    pending_depth_owner: Option<i64>,
    pending_flow_owner: Option<i64>,
    pending_subject_owner: Option<i64>,
    dnn_backend_initialized: bool,
    dnn_backend_available: bool,
    dnn_next_retry_frame: i64,
    warned_missing_dnn: bool,
    dnn_subject_api_available: bool,
    // WireframeDepthFX.cs line 102 — static ompEnvConfigured
    // Handled in FfiDepthEstimator::new() — KMP_DUPLICATE_LIB_OK set there.
    // --- hal dual-path resources (pipeline + sampler for each of 15 passes) ---
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_pipelines: Option<Vec<crate::hal_pipeline::HalRenderPipeline>>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_sampler: Option<crate::hal_context::MetalSampler>,
    /// 15 hal compute pipelines (one per entry point) for TBDR-bypass dispatch.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_compute_pipelines: Option<Vec<crate::hal_pipeline::HalComputePipeline>>,
    /// Persistent mapped pointer to shared-memory ring buffer (hal path).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_uniform_mapped_ptr: Option<*mut u8>,
    /// Cached hal buffer pointer for the ring buffer (hal path).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_ring_buf_ptr: Option<*const crate::hal_context::MetalBuffer>,
    /// Frame-local slot counter for the ring buffer (Cell so encode_pass can stay &self).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_ring_offset: std::cell::Cell<u32>,
}

#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
unsafe impl Send for WireframeDepthFX {}

impl WireframeDepthFX {
    pub fn new(
        device: &wgpu::Device,
        hal_ctx: Option<&crate::hal_context::HalContext>,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("WireframeDepth"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fx_wireframe_depth.wgsl").into(),
            ),
        });

        // Bind group layout: uniforms + 13 textures + 1 sampler (binding 0–13).
        // Layout matches shader: bindings 0=uniforms, 1–12=textures, 13=sampler.
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("WireframeDepth BGL"),
            entries: &[
                // 0: uniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // 1–12: texture_2d<f32> (filterable float)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 9,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 10,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 12,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // 13: sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 13,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("WireframeDepth PL"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        // Fragment entry points — indexed by pass constant.
        // WireframeDepthEffect.shader pass order (0–14).
        let entry_points = [
            "fs_analysis",           // 0
            "fs_heuristic_depth",    // 1
            "fs_wire_mask",          // 2
            "fs_update_history",     // 3
            "fs_composite",          // 4
            "fs_dnn_depth_post",     // 5
            "fs_flow_estimate",      // 6
            "fs_flow_advect_coord",  // 7
            "fs_init_mesh_coord",    // 8
            "fs_mesh_regularize",    // 9
            "fs_mesh_cell_affine",   // 10
            "fs_semantic_mask",      // 11
            "fs_mesh_face_warp",     // 12
            "fs_surface_cache_update", // 13
            "fs_flow_hygiene",       // 14
        ];

        // Output formats per pass.
        // analysis → Rgba8Unorm  (ARGB32)
        // heuristic_depth → Rgba16Float (ARGBHalf)
        // wire_mask → Rgba8Unorm (ARGB32)
        // update_history → Rgba8Unorm (ARGB32)
        // composite → Rgba16Float (source frame format)
        // dnn_depth_post → Rgba16Float (ARGBHalf)
        // flow_estimate → Rgba16Float (ARGBHalf)
        // flow_advect_coord → Rgba16Float (ARGBHalf)
        // init_mesh_coord → Rgba16Float (ARGBHalf)
        // mesh_regularize → Rgba16Float (ARGBHalf)
        // mesh_cell_affine → Rgba16Float (ARGBHalf)
        // semantic_mask → Rgba16Float (ARGBHalf)
        // mesh_face_warp → Rgba16Float (ARGBHalf)
        // surface_cache_update → Rgba16Float (ARGBHalf)
        // flow_hygiene → Rgba16Float (ARGBHalf)
        let output_formats: [wgpu::TextureFormat; 15] = [
            wgpu::TextureFormat::Rgba8Unorm,   // 0  ARGB32
            wgpu::TextureFormat::Rgba16Float,  // 1  ARGBHalf
            wgpu::TextureFormat::Rgba8Unorm,   // 2  ARGB32
            wgpu::TextureFormat::Rgba8Unorm,   // 3  ARGB32
            wgpu::TextureFormat::Rgba16Float,  // 4  source frame
            wgpu::TextureFormat::Rgba16Float,  // 5  ARGBHalf
            wgpu::TextureFormat::Rgba16Float,  // 6  ARGBHalf
            wgpu::TextureFormat::Rgba16Float,  // 7  ARGBHalf
            wgpu::TextureFormat::Rgba16Float,  // 8  ARGBHalf
            wgpu::TextureFormat::Rgba16Float,  // 9  ARGBHalf
            wgpu::TextureFormat::Rgba16Float,  // 10 ARGBHalf
            wgpu::TextureFormat::Rgba16Float,  // 11 ARGBHalf
            wgpu::TextureFormat::Rgba16Float,  // 12 ARGBHalf
            wgpu::TextureFormat::Rgba16Float,  // 13 ARGBHalf
            wgpu::TextureFormat::Rgba16Float,  // 14 ARGBHalf
        ];

        let pipelines: [wgpu::RenderPipeline; 15] = std::array::from_fn(|i| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&format!("WireframeDepth P{i}")),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry_points[i]),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: output_formats[i],
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("WireframeDepth Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let ubo_size = std::mem::size_of::<WireUniforms>() as u64;
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let (uniform_buffer, _wd_hal_ubo_mapped, _wd_hal_ring_buf_ptr) = if let Some(ctx) = hal_ctx {
            use wgpu::hal::Device as HalDevice;
            // 20 slots: enough for all passes per frame (15 pass max + sub-function calls).
            let ring_size = 20 * ubo_size;
            let hal_buf = unsafe {
                ctx.device()
                    .create_buffer(&wgpu::hal::BufferDescriptor {
                        label: Some("WireframeDepth Ring HAL"),
                        size: ring_size,
                        usage: wgpu::wgt::BufferUses::UNIFORM
                            | wgpu::wgt::BufferUses::MAP_WRITE,
                        memory_flags: wgpu::hal::MemoryFlags::PREFER_COHERENT,
                    })
                    .expect("Failed to create hal WD ring buffer")
            };
            let mapping = unsafe {
                ctx.device()
                    .map_buffer(&hal_buf, 0..ring_size)
                    .expect("Failed to map hal WD ring buffer")
            };
            let mapped_ptr = mapping.ptr.as_ptr();
            let wgpu_buf = unsafe {
                device.create_buffer_from_hal::<wgpu::hal::api::Metal>(
                    hal_buf,
                    &wgpu::BufferDescriptor {
                        label: Some("WireframeDepth Ring UBO"),
                        size: ring_size,
                        usage: wgpu::BufferUsages::UNIFORM
                            | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    },
                )
            };
            let hal_ring_buf_ptr = {
                let g = unsafe { wgpu_buf.as_hal::<wgpu::hal::api::Metal>() }
                    .expect("ring buf not Metal");
                &*g as *const _
            };
            (wgpu_buf, Some(mapped_ptr), Some(hal_ring_buf_ptr))
        } else {
            let wgpu_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("WireframeDepth UBO"),
                size: ubo_size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            (wgpu_buf, None, None)
        };
        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("WireframeDepth UBO"),
            size: ubo_size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("WireframeDepth Dummy"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let dummy_view = dummy_tex.create_view(&wgpu::TextureViewDescriptor::default());

        // WireframeDepthFX.cs line 96-101 — try to create native backend
        // Plugin is created on the worker thread (single creation, no probe).
        let workers = Self::try_spawn_workers();
        let dnn_backend_available = workers.is_some();
        let dnn_backend_initialized = workers.is_some();

        // --- hal pipeline creation (one per pass × 15 output formats) ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let (hal_pipelines, hal_sampler, hal_compute_pipelines) = if let Some(ctx) = hal_ctx {
            use wgpu::hal::Device as HalDevice;

            let wgsl_source = include_str!("shaders/fx_wireframe_depth.wgsl");
            let hal_pipes: Vec<crate::hal_pipeline::HalRenderPipeline> = entry_points
                .iter()
                .zip(output_formats.iter())
                .enumerate()
                .map(|(i, (fs_ep, &fmt))| {
                    crate::hal_pipeline::create_render_pipeline(
                        ctx,
                        wgsl_source,
                        "vs_main",
                        fs_ep,
                        &WIREFRAME_DEPTH_BGL_ENTRIES,
                        fmt,
                        &format!("WireframeDepth HAL P{i}"),
                    )
                })
                .collect();

            let hal_samp = unsafe {
                ctx.device()
                    .create_sampler(&wgpu::hal::SamplerDescriptor {
                        label: Some("WireframeDepth HAL"),
                        address_modes: [wgpu::AddressMode::ClampToEdge; 3],
                        mag_filter: wgpu::FilterMode::Linear,
                        min_filter: wgpu::FilterMode::Linear,
                        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                        lod_clamp: 0.0..32.0,
                        compare: None,
                        anisotropy_clamp: 1,
                        border_color: None,
                    })
                    .expect("Failed to create hal wireframe-depth sampler")
            };

            // --- hal compute pipelines (15 entry points, TBDR-bypass) ---
            let compute_wgsl = include_str!("shaders/fx_wireframe_depth_compute.wgsl");
            let cs_entry_points = [
                "cs_analysis",              // 0
                "cs_heuristic_depth",       // 1
                "cs_wire_mask",             // 2
                "cs_update_history",        // 3
                "cs_composite",             // 4
                "cs_dnn_depth_post",        // 5
                "cs_flow_estimate",         // 6
                "cs_flow_advect_coord",     // 7
                "cs_init_mesh_coord",       // 8
                "cs_mesh_regularize",       // 9
                "cs_mesh_cell_affine",      // 10
                "cs_semantic_mask",         // 11
                "cs_mesh_face_warp",        // 12
                "cs_surface_cache_update",  // 13
                "cs_flow_hygiene",          // 14
            ];
            let hal_cs_pipes: Vec<crate::hal_pipeline::HalComputePipeline> = cs_entry_points
                .iter()
                .enumerate()
                .map(|(i, ep)| {
                    crate::hal_pipeline::create_compute_pipeline(
                        ctx,
                        compute_wgsl,
                        ep,
                        &WIREFRAME_DEPTH_COMPUTE_BGL_ENTRIES,
                        &format!("WireframeDepth CS HAL P{i}"),
                    )
                })
                .collect();

            (Some(hal_pipes), Some(hal_samp), Some(hal_cs_pipes))
        } else {
            (None, None, None)
        };

        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        let _ = &hal_ctx;

        Self {
            pipelines,
            bind_group_layout,
            sampler,
            uniform_buffer,
            _dummy_tex: dummy_tex,
            dummy_view,
            owner_states: AHashMap::new(),
            width: 0,
            height: 0,
            workers,
            pending_depth_owner: None,
            pending_flow_owner: None,
            pending_subject_owner: None,
            dnn_backend_initialized,
            dnn_backend_available,
            dnn_next_retry_frame: 0,
            warned_missing_dnn: false,
            dnn_subject_api_available: true,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_pipelines,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_sampler,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_compute_pipelines,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_uniform_mapped_ptr: _wd_hal_ubo_mapped,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_ring_buf_ptr: _wd_hal_ring_buf_ptr,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_ring_offset: std::cell::Cell::new(0),
        }
    }

    // WireframeDepthFX.cs line 259-268 — CreateRenderTexture helper
    fn create_rt(
        device: &wgpu::Device,
        w: u32,
        h: u32,
        format: wgpu::TextureFormat,
        label: &str,
    ) -> RenderTarget {
        RenderTarget::new(device, w, h, format, label)
    }

    // WireframeDepthFX.cs line 270-277 — ClearRenderTexture: write black pixels via queue clear
    fn clear_rt(encoder: &mut wgpu::CommandEncoder, rt: &RenderTarget) {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("WireframeDepth Clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &rt.view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }

    /// HAL path: encode one render pass via hal command encoder.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(clippy::too_many_arguments)]
    fn run_pass_hal(
        &self,
        gpu: &mut GpuEncoder,
        pass_idx: usize,
        uniforms: &WireUniforms,
        main_view: &wgpu::TextureView,
        prev_analysis_view: &wgpu::TextureView,
        prev_depth_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
        history_view: &wgpu::TextureView,
        flow_view: &wgpu::TextureView,
        mesh_coord_view: &wgpu::TextureView,
        prev_mesh_coord_view: &wgpu::TextureView,
        semantic_view: &wgpu::TextureView,
        surface_cache_view: &wgpu::TextureView,
        prev_surface_cache_view: &wgpu::TextureView,
        subject_mask_view: &wgpu::TextureView,
        target: &wgpu::TextureView,
        w: u32,
        h: u32,
    ) {
        type MetalApi = wgpu::hal::api::Metal;
        use wgpu::hal::{self as hal, CommandEncoder as _, Device as _};

        let hal_pipes = self.hal_pipelines.as_ref().expect("hal_pipelines");
        let hal_samp = self.hal_sampler.as_ref().expect("hal_sampler");
        let hal_pipe = &hal_pipes[pass_idx];

        // Write uniforms to ring buffer slot
        let ubo_size = std::mem::size_of::<WireUniforms>();
        let slot = self.hal_ring_offset.get();
        self.hal_ring_offset.set(slot + 1);
        let byte_offset = slot as usize * ubo_size;
        let ring_mapped = self.hal_uniform_mapped_ptr.expect("hal ring mapped");
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytemuck::bytes_of(uniforms).as_ptr(),
                ring_mapped.add(byte_offset),
                ubo_size,
            );
        }
        let ubo_ptr = self.hal_ring_buf_ptr.expect("hal ring buf ptr");

        // Extract hal texture view pointers (sequential snatch lock)
        let main_ptr = {
            let g = unsafe { main_view.as_hal::<MetalApi>() }
                .expect("main_view not Metal");
            &*g as *const _
        };
        let prev_analysis_ptr = {
            let g = unsafe { prev_analysis_view.as_hal::<MetalApi>() }
                .expect("prev_analysis not Metal");
            &*g as *const _
        };
        let prev_depth_ptr = {
            let g = unsafe { prev_depth_view.as_hal::<MetalApi>() }
                .expect("prev_depth not Metal");
            &*g as *const _
        };
        let depth_ptr = {
            let g = unsafe { depth_view.as_hal::<MetalApi>() }
                .expect("depth not Metal");
            &*g as *const _
        };
        let history_ptr = {
            let g = unsafe { history_view.as_hal::<MetalApi>() }
                .expect("history not Metal");
            &*g as *const _
        };
        let flow_ptr = {
            let g = unsafe { flow_view.as_hal::<MetalApi>() }
                .expect("flow not Metal");
            &*g as *const _
        };
        let mesh_coord_ptr = {
            let g = unsafe { mesh_coord_view.as_hal::<MetalApi>() }
                .expect("mesh_coord not Metal");
            &*g as *const _
        };
        let prev_mesh_coord_ptr = {
            let g = unsafe { prev_mesh_coord_view.as_hal::<MetalApi>() }
                .expect("prev_mesh_coord not Metal");
            &*g as *const _
        };
        let semantic_ptr = {
            let g = unsafe { semantic_view.as_hal::<MetalApi>() }
                .expect("semantic not Metal");
            &*g as *const _
        };
        let surface_cache_ptr = {
            let g = unsafe { surface_cache_view.as_hal::<MetalApi>() }
                .expect("surface_cache not Metal");
            &*g as *const _
        };
        let prev_surface_cache_ptr = {
            let g = unsafe { prev_surface_cache_view.as_hal::<MetalApi>() }
                .expect("prev_surface_cache not Metal");
            &*g as *const _
        };
        let subject_mask_ptr = {
            let g = unsafe { subject_mask_view.as_hal::<MetalApi>() }
                .expect("subject_mask not Metal");
            &*g as *const _
        };
        let target_ptr = {
            let g = unsafe { target.as_hal::<MetalApi>() }
                .expect("target not Metal");
            &*g as *const _
        };

        let (hal_enc, hal_ctx) = unsafe { gpu.hal_encoder_mut() }.unwrap();
        unsafe {
            let hal_bg = hal_ctx.device().create_bind_group(
                &hal::BindGroupDescriptor {
                    label: None,
                    layout: &hal_pipe.bind_group_layout,
                    entries: &[
                        hal::BindGroupEntry { binding: 0, resource_index: 0, count: 1 },
                        hal::BindGroupEntry { binding: 1, resource_index: 0, count: 1 },
                        hal::BindGroupEntry { binding: 2, resource_index: 1, count: 1 },
                        hal::BindGroupEntry { binding: 3, resource_index: 2, count: 1 },
                        hal::BindGroupEntry { binding: 4, resource_index: 3, count: 1 },
                        hal::BindGroupEntry { binding: 5, resource_index: 4, count: 1 },
                        hal::BindGroupEntry { binding: 6, resource_index: 5, count: 1 },
                        hal::BindGroupEntry { binding: 7, resource_index: 6, count: 1 },
                        hal::BindGroupEntry { binding: 8, resource_index: 7, count: 1 },
                        hal::BindGroupEntry { binding: 9, resource_index: 8, count: 1 },
                        hal::BindGroupEntry { binding: 10, resource_index: 9, count: 1 },
                        hal::BindGroupEntry { binding: 11, resource_index: 10, count: 1 },
                        hal::BindGroupEntry { binding: 12, resource_index: 11, count: 1 },
                        hal::BindGroupEntry { binding: 13, resource_index: 0, count: 1 },
                    ],
                    buffers: &[hal::BufferBinding::new_unchecked(
                        &*ubo_ptr,
                        byte_offset as u64,
                        std::num::NonZero::new(
                            std::mem::size_of::<WireUniforms>() as u64,
                        ),
                    )],
                    samplers: &[hal_samp],
                    textures: &[
                        hal::TextureBinding { view: &*main_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*prev_analysis_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*prev_depth_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*depth_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*history_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*flow_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*mesh_coord_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*prev_mesh_coord_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*semantic_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*surface_cache_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*prev_surface_cache_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*subject_mask_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                    ],
                    acceleration_structures: &[],
                    external_textures: &[],
                },
            )
            .expect("Failed to create hal wireframe-depth bind group");

            hal_enc.begin_render_pass(&hal::RenderPassDescriptor {
                label: None,
                extent: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                sample_count: 1,
                color_attachments: &[Some(hal::ColorAttachment {
                    target: hal::Attachment {
                        view: &*target_ptr,
                        usage: wgpu::wgt::TextureUses::COLOR_TARGET,
                    },
                    resolve_target: None,
                    ops: hal::AttachmentOps::LOAD_CLEAR | hal::AttachmentOps::STORE,
                    clear_value: wgpu::Color::TRANSPARENT,
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                multiview_mask: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            }).expect("hal begin_render_pass failed");
            hal_enc.set_render_pipeline(&hal_pipe.pipeline);
            hal_enc.set_bind_group(
                &hal_pipe.pipeline_layout, 0, &hal_bg,
                &[],
            );
            hal_enc.draw(0, 3, 0, 1);
            hal_enc.end_render_pass();
            hal_ctx.device().destroy_bind_group(hal_bg);
        }
    }

    /// HAL compute path: dispatch a compute shader instead of a render pass.
    /// Eliminates TBDR tile load/store overhead (~290us per pass at 4K).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(clippy::too_many_arguments)]
    fn run_pass_compute_hal(
        &self,
        gpu: &mut GpuEncoder,
        pass_idx: usize,
        uniforms: &WireUniforms,
        main_view: &wgpu::TextureView,
        prev_analysis_view: &wgpu::TextureView,
        prev_depth_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
        history_view: &wgpu::TextureView,
        flow_view: &wgpu::TextureView,
        mesh_coord_view: &wgpu::TextureView,
        prev_mesh_coord_view: &wgpu::TextureView,
        semantic_view: &wgpu::TextureView,
        surface_cache_view: &wgpu::TextureView,
        prev_surface_cache_view: &wgpu::TextureView,
        subject_mask_view: &wgpu::TextureView,
        target: &wgpu::TextureView,
        w: u32,
        h: u32,
    ) {
        type MetalApi = wgpu::hal::api::Metal;
        use wgpu::hal::{self as hal, CommandEncoder as _, Device as _};

        let hal_cs_pipes = self.hal_compute_pipelines.as_ref().expect("hal_compute_pipelines");
        let hal_samp = self.hal_sampler.as_ref().expect("hal_sampler");
        let hal_cs_pipe = &hal_cs_pipes[pass_idx];

        // Write uniforms to ring buffer slot (same mechanism as render path)
        let ubo_size = std::mem::size_of::<WireUniforms>();
        let slot = self.hal_ring_offset.get();
        self.hal_ring_offset.set(slot + 1);
        let byte_offset = slot as usize * ubo_size;
        let ring_mapped = self.hal_uniform_mapped_ptr.expect("hal ring mapped");
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytemuck::bytes_of(uniforms).as_ptr(),
                ring_mapped.add(byte_offset),
                ubo_size,
            );
        }
        let ubo_ptr = self.hal_ring_buf_ptr.expect("hal ring buf ptr");

        // Extract hal texture view pointers (sequential snatch lock)
        let main_ptr = {
            let g = unsafe { main_view.as_hal::<MetalApi>() }
                .expect("main_view not Metal");
            &*g as *const _
        };
        let prev_analysis_ptr = {
            let g = unsafe { prev_analysis_view.as_hal::<MetalApi>() }
                .expect("prev_analysis not Metal");
            &*g as *const _
        };
        let prev_depth_ptr = {
            let g = unsafe { prev_depth_view.as_hal::<MetalApi>() }
                .expect("prev_depth not Metal");
            &*g as *const _
        };
        let depth_ptr = {
            let g = unsafe { depth_view.as_hal::<MetalApi>() }
                .expect("depth not Metal");
            &*g as *const _
        };
        let history_ptr = {
            let g = unsafe { history_view.as_hal::<MetalApi>() }
                .expect("history not Metal");
            &*g as *const _
        };
        let flow_ptr = {
            let g = unsafe { flow_view.as_hal::<MetalApi>() }
                .expect("flow not Metal");
            &*g as *const _
        };
        let mesh_coord_ptr = {
            let g = unsafe { mesh_coord_view.as_hal::<MetalApi>() }
                .expect("mesh_coord not Metal");
            &*g as *const _
        };
        let prev_mesh_coord_ptr = {
            let g = unsafe { prev_mesh_coord_view.as_hal::<MetalApi>() }
                .expect("prev_mesh_coord not Metal");
            &*g as *const _
        };
        let semantic_ptr = {
            let g = unsafe { semantic_view.as_hal::<MetalApi>() }
                .expect("semantic not Metal");
            &*g as *const _
        };
        let surface_cache_ptr = {
            let g = unsafe { surface_cache_view.as_hal::<MetalApi>() }
                .expect("surface_cache not Metal");
            &*g as *const _
        };
        let prev_surface_cache_ptr = {
            let g = unsafe { prev_surface_cache_view.as_hal::<MetalApi>() }
                .expect("prev_surface_cache not Metal");
            &*g as *const _
        };
        let subject_mask_ptr = {
            let g = unsafe { subject_mask_view.as_hal::<MetalApi>() }
                .expect("subject_mask not Metal");
            &*g as *const _
        };
        let target_ptr = {
            let g = unsafe { target.as_hal::<MetalApi>() }
                .expect("target not Metal");
            &*g as *const _
        };

        let (hal_enc, hal_ctx) = unsafe { gpu.hal_encoder_mut() }.unwrap();
        unsafe {
            // Compute BGL: 0=uniform, 1-12=textures (RESOURCE), 13=sampler,
            // 14=output storage texture (STORAGE_READ_WRITE)
            let hal_bg = hal_ctx.device().create_bind_group(
                &hal::BindGroupDescriptor {
                    label: None,
                    layout: &hal_cs_pipe.bind_group_layout,
                    entries: &[
                        hal::BindGroupEntry { binding: 0, resource_index: 0, count: 1 },
                        hal::BindGroupEntry { binding: 1, resource_index: 0, count: 1 },
                        hal::BindGroupEntry { binding: 2, resource_index: 1, count: 1 },
                        hal::BindGroupEntry { binding: 3, resource_index: 2, count: 1 },
                        hal::BindGroupEntry { binding: 4, resource_index: 3, count: 1 },
                        hal::BindGroupEntry { binding: 5, resource_index: 4, count: 1 },
                        hal::BindGroupEntry { binding: 6, resource_index: 5, count: 1 },
                        hal::BindGroupEntry { binding: 7, resource_index: 6, count: 1 },
                        hal::BindGroupEntry { binding: 8, resource_index: 7, count: 1 },
                        hal::BindGroupEntry { binding: 9, resource_index: 8, count: 1 },
                        hal::BindGroupEntry { binding: 10, resource_index: 9, count: 1 },
                        hal::BindGroupEntry { binding: 11, resource_index: 10, count: 1 },
                        hal::BindGroupEntry { binding: 12, resource_index: 11, count: 1 },
                        hal::BindGroupEntry { binding: 13, resource_index: 0, count: 1 },
                        hal::BindGroupEntry { binding: 14, resource_index: 12, count: 1 },
                    ],
                    buffers: &[hal::BufferBinding::new_unchecked(
                        &*ubo_ptr,
                        byte_offset as u64,
                        std::num::NonZero::new(
                            std::mem::size_of::<WireUniforms>() as u64,
                        ),
                    )],
                    samplers: &[hal_samp],
                    textures: &[
                        hal::TextureBinding { view: &*main_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*prev_analysis_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*prev_depth_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*depth_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*history_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*flow_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*mesh_coord_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*prev_mesh_coord_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*semantic_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*surface_cache_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*prev_surface_cache_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*subject_mask_ptr, usage: wgpu::wgt::TextureUses::RESOURCE },
                        hal::TextureBinding { view: &*target_ptr, usage: wgpu::wgt::TextureUses::STORAGE_READ_WRITE },
                    ],
                    acceleration_structures: &[],
                    external_textures: &[],
                },
            )
            .expect("Failed to create hal wireframe-depth compute bind group");

            hal_enc.begin_compute_pass(&hal::ComputePassDescriptor {
                label: Some("WireframeDepth Compute"),
                timestamp_writes: None,
            });
            hal_enc.set_compute_pipeline(&hal_cs_pipe.pipeline);
            hal_enc.set_bind_group(
                &hal_cs_pipe.pipeline_layout, 0, &hal_bg,
                &[],
            );
            hal_enc.dispatch([w.div_ceil(16), h.div_ceil(16), 1]);
            hal_enc.end_compute_pass();
            hal_ctx.device().destroy_bind_group(hal_bg);
        }
    }

    /// HAL path: copy texture to texture via hal command encoder.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    fn copy_texture_hal(
        gpu: &mut GpuEncoder,
        src: &wgpu::Texture,
        dst: &wgpu::Texture,
        w: u32,
        h: u32,
    ) {
        type MetalApi = wgpu::hal::api::Metal;
        use wgpu::hal::CommandEncoder as _;

        let src_ptr = {
            let g = unsafe { src.as_hal::<MetalApi>() }
                .expect("src tex not Metal");
            &*g as *const _
        };
        let dst_ptr = {
            let g = unsafe { dst.as_hal::<MetalApi>() }
                .expect("dst tex not Metal");
            &*g as *const _
        };
        let (hal_enc, _) = unsafe { gpu.hal_encoder_mut() }.unwrap();
        unsafe {
            hal_enc.copy_texture_to_texture(
                &*src_ptr,
                wgpu::wgt::TextureUses::COPY_SRC,
                &*dst_ptr,
                std::iter::once(wgpu::hal::TextureCopy {
                    src_base: wgpu::hal::TextureCopyBase {
                        mip_level: 0,
                        array_layer: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::hal::FormatAspects::COLOR,
                    },
                    dst_base: wgpu::hal::TextureCopyBase {
                        mip_level: 0,
                        array_layer: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::hal::FormatAspects::COLOR,
                    },
                    size: wgpu::hal::CopyExtent {
                        width: w,
                        height: h,
                        depth: 1,
                    },
                }),
            );
        }
    }

    /// HAL path: clear a RenderTarget to black via hal render pass.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    fn clear_rt_hal(gpu: &mut GpuEncoder, rt: &RenderTarget) {
        type MetalApi = wgpu::hal::api::Metal;
        use wgpu::hal::{self as hal, CommandEncoder as _};

        let view_ptr = {
            let g = unsafe { rt.view.as_hal::<MetalApi>() }
                .expect("rt view not Metal");
            &*g as *const _
        };
        let (hal_enc, _) = unsafe { gpu.hal_encoder_mut() }.unwrap();
        unsafe {
            hal_enc.begin_render_pass(&hal::RenderPassDescriptor {
                label: None,
                extent: wgpu::Extent3d {
                    width: rt.width, height: rt.height,
                    depth_or_array_layers: 1,
                },
                sample_count: 1,
                color_attachments: &[Some(hal::ColorAttachment {
                    target: hal::Attachment {
                        view: &*view_ptr,
                        usage: wgpu::wgt::TextureUses::COLOR_TARGET,
                    },
                    resolve_target: None,
                    ops: hal::AttachmentOps::LOAD_CLEAR | hal::AttachmentOps::STORE,
                    clear_value: wgpu::Color::BLACK,
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                multiview_mask: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            }).expect("hal begin_render_pass failed");
            hal_enc.end_render_pass();
        }
    }

    /// Unified encode: compute dispatch (preferred) → hal render pass → wgpu render pass.
    /// Compute dispatch eliminates TBDR tile overhead (~290us/pass at 4K).
    #[allow(clippy::too_many_arguments)]
    fn encode_pass(
        &self,
        gpu: &mut GpuEncoder,
        pass_idx: usize,
        uniforms: &WireUniforms,
        main_view: &wgpu::TextureView,
        prev_analysis_view: &wgpu::TextureView,
        prev_depth_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
        history_view: &wgpu::TextureView,
        flow_view: &wgpu::TextureView,
        mesh_coord_view: &wgpu::TextureView,
        prev_mesh_coord_view: &wgpu::TextureView,
        semantic_view: &wgpu::TextureView,
        surface_cache_view: &wgpu::TextureView,
        prev_surface_cache_view: &wgpu::TextureView,
        subject_mask_view: &wgpu::TextureView,
        target: &wgpu::TextureView,
        w: u32,
        h: u32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if gpu.has_hal_encoder() {
            // Prefer compute dispatch (eliminates TBDR tile overhead)
            if self.hal_compute_pipelines.is_some() {
                self.run_pass_compute_hal(
                    gpu, pass_idx, uniforms,
                    main_view, prev_analysis_view, prev_depth_view, depth_view,
                    history_view, flow_view, mesh_coord_view, prev_mesh_coord_view,
                    semantic_view, surface_cache_view, prev_surface_cache_view,
                    subject_mask_view, target, w, h,
                );
                return;
            }
            // Fallback: hal render pass
            self.run_pass_hal(
                gpu, pass_idx, uniforms,
                main_view, prev_analysis_view, prev_depth_view, depth_view,
                history_view, flow_view, mesh_coord_view, prev_mesh_coord_view,
                semantic_view, surface_cache_view, prev_surface_cache_view,
                subject_mask_view, target, w, h,
            );
            return;
        }
        // wgpu path: create per-call buffer from uniforms
        let ubo = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("WD Pass UBO"),
            contents: bytemuck::bytes_of(uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bg = self.make_bind_group(
            gpu.device, &ubo,
            main_view, prev_analysis_view, prev_depth_view, depth_view,
            history_view, flow_view, mesh_coord_view, prev_mesh_coord_view,
            semantic_view, surface_cache_view, prev_surface_cache_view,
            subject_mask_view,
        );
        self.run_pass(
            gpu.encoder, &self.pipelines[pass_idx], &bg, target, w, h, profiler,
        );
    }

    /// Unified encode: copy texture to texture via hal or wgpu.
    fn encode_copy(
        gpu: &mut GpuEncoder,
        src: &wgpu::Texture,
        dst: &wgpu::Texture,
        w: u32,
        h: u32,
    ) {
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if gpu.has_hal_encoder() {
            Self::copy_texture_hal(gpu, src, dst, w, h);
            return;
        }
        gpu.encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: src,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: dst,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }

    /// Unified encode: clear a RenderTarget to black via hal or wgpu.
    fn encode_clear(gpu: &mut GpuEncoder, rt: &RenderTarget) {
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if gpu.has_hal_encoder() {
            Self::clear_rt_hal(gpu, rt);
            return;
        }
        Self::clear_rt(gpu.encoder, rt);
    }

    // Create a CPU-upload 2D texture (Rgba8Unorm or Rgba32Float) for DNN outputs.
    fn create_cpu_texture(
        device: &wgpu::Device,
        w: u32,
        h: u32,
        format: wgpu::TextureFormat,
        label: &str,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        (tex, view)
    }

    // WireframeDepthFX.cs line 139-238 — GetOrCreateOwner
    fn get_or_create_owner(
        &mut self,
        gpu: &mut GpuEncoder,
        owner_key: i64,
        wire_scale: f32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) -> &mut OwnerState {
        // WireframeDepthFX.cs line 141-142
        let wire_w = (self.width as f32 * wire_scale).round() as u32;
        let wire_w = wire_w.max(64);
        let wire_h = (self.height as f32 * wire_scale).round() as u32;
        let wire_h = wire_h.max(36);

        // WireframeDepthFX.cs line 144-162: if exists and valid, rebuild wire RT only if scale changed
        if let Some(state) = self.owner_states.get_mut(&owner_key) {
            if state.wire_width != wire_w || state.wire_height != wire_h {
                // Rebuild line history RT only
                state.wire_width = wire_w;
                state.wire_height = wire_h;
                state.line_history_tex = Self::create_rt(
                    gpu.device, wire_w, wire_h,
                    wgpu::TextureFormat::Rgba8Unorm,
                    &format!("WireframeDepthHistory_{owner_key}"),
                );
                Self::encode_clear(gpu, &state.line_history_tex);
            }
            // Rust borrow checker: re-borrow mutably after the if-chain
            return self.owner_states.get_mut(&owner_key).unwrap();
        }

        // WireframeDepthFX.cs line 164-165: release stale state (handled by drop on overwrite below)

        // WireframeDepthFX.cs line 167-169
        let scale = (MAX_ANALYSIS_DIM as f32 / self.width.max(self.height) as f32).min(1.0);
        let analysis_width  = ((self.width  as f32 * scale).round() as u32).max(64);
        let analysis_height = ((self.height as f32 * scale).round() as u32).max(36);

        let aw = analysis_width;
        let ah = analysis_height;
        let pixel_count = (aw * ah) as usize;

        let previous_analysis_tex = Self::create_rt(
            gpu.device, aw, ah, wgpu::TextureFormat::Rgba8Unorm,
            &format!("WireframeDepthPrev_{owner_key}"));
        let depth_tex = Self::create_rt(
            gpu.device, aw, ah, wgpu::TextureFormat::Rgba16Float,
            &format!("WireframeDepthDepth_{owner_key}"));
        let line_history_tex = Self::create_rt(
            gpu.device, wire_w, wire_h, wgpu::TextureFormat::Rgba8Unorm,
            &format!("WireframeDepthHistory_{owner_key}"));
        let flow_tex = Self::create_rt(
            gpu.device, aw, ah, wgpu::TextureFormat::Rgba16Float,
            &format!("WireframeDepthFlow_{owner_key}"));
        let mesh_coord_tex = Self::create_rt(
            gpu.device, aw, ah, wgpu::TextureFormat::Rgba16Float,
            &format!("WireframeDepthMeshCoord_{owner_key}"));
        let semantic_tex = Self::create_rt(
            gpu.device, aw, ah, wgpu::TextureFormat::Rgba16Float,
            &format!("WireframeDepthSemantic_{owner_key}"));
        let surface_cache_tex = Self::create_rt(
            gpu.device, aw, ah, wgpu::TextureFormat::Rgba16Float,
            &format!("WireframeDepthSurface_{owner_key}"));
        let dnn_input_tex = Self::create_rt(
            gpu.device, aw, ah, wgpu::TextureFormat::Rgba8Unorm,
            &format!("WireframeDepthDnnInput_{owner_key}"));

        // WireframeDepthFX.cs line 205-222 — CPU upload textures
        let (dnn_depth_texture, dnn_depth_texture_view) = Self::create_cpu_texture(
            gpu.device, aw, ah, wgpu::TextureFormat::Rgba8Unorm,
            &format!("WireframeDepthDnnDepth_{owner_key}"));
        // Unity: RGBAFloat (Rgba32Float), but Rgba32Float is NOT filterable on Metal.
        // textureSample requires filterable; Rgba16Float is the approved Metal fallback.
        // Upload converts f32 → f16 in upload_native_flow_texture().
        let (native_flow_texture, native_flow_texture_view) = Self::create_cpu_texture(
            gpu.device, aw, ah, wgpu::TextureFormat::Rgba16Float,
            &format!("WireframeDepthNativeFlow_{owner_key}"));
        let (dnn_subject_texture, dnn_subject_texture_view) = Self::create_cpu_texture(
            gpu.device, aw, ah, wgpu::TextureFormat::Rgba8Unorm,
            &format!("WireframeDepthDnnSubject_{owner_key}"));

        // WireframeDepthFX.cs line 224-231: clear RTs
        Self::encode_clear(gpu, &previous_analysis_tex);
        Self::encode_clear(gpu, &depth_tex);
        Self::encode_clear(gpu, &line_history_tex);
        Self::encode_clear(gpu, &flow_tex);
        Self::encode_clear(gpu, &semantic_tex);
        Self::encode_clear(gpu, &surface_cache_tex);
        Self::encode_clear(gpu, &dnn_input_tex);

        let mut state = OwnerState {
            analysis_width:  aw,
            analysis_height: ah,
            wire_width:  wire_w,
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
            dnn_depth_texture_view,
            dnn_has_subject_mask: false,
            dnn_subject_dirty: false,
            _dnn_subject_buffer: vec![0.0f32; pixel_count],
            dnn_subject_history_buffer: vec![0.0f32; pixel_count],
            dnn_subject_texture,
            dnn_subject_texture_view,
            has_prev_native_frame: false,
            prev_native_pixel_buffer: vec![0u8; pixel_count * 4],
            native_flow_buffer: vec![0.0f32; pixel_count * 4],
            native_flow_texture,
            native_flow_texture_view,
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
        self.initialize_mesh_coord_new(gpu, &mut state, profiler);

        self.owner_states.insert(owner_key, state);
        self.owner_states.get_mut(&owner_key).unwrap()
    }

    // WireframeDepthFX.cs line 240-257 — InitializeMeshCoord
    // Called during owner creation. Runs PASS_INIT_MESH_COORD then PASS_SURFACE_CACHE_UPDATE.
    fn initialize_mesh_coord_new(
        &self,
        gpu: &mut GpuEncoder,
        state: &mut OwnerState,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
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
        // In Unity: Graphics.Blit(null, state.meshCoordTex, material, PASS_INIT_MESH_COORD)
        // We bind dummy for all textures.
        self.encode_pass(gpu, PASS_INIT_MESH_COORD, &uniforms,
            &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &state.mesh_coord_tex.view, aw, ah, profiler);
        // PASS_SURFACE_CACHE_UPDATE from fresh mesh coord
        self.encode_pass(gpu, PASS_SURFACE_CACHE_UPDATE, &uniforms,
            &state.mesh_coord_tex.view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &state.surface_cache_tex.view, aw, ah, profiler);
    }

    // Helper: encode a single render pass (blit-style, no vertex buffer).
    fn run_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        bind_group: &wgpu::BindGroup,
        target: &wgpu::TextureView,
        w: u32,
        h: u32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        let ts = profiler.and_then(|p| p.render_timestamps("WireframeDepth Pass", w, h));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: ts,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    // Helper: run a pass writing to a temporary RenderTarget.
    #[allow(dead_code, clippy::too_many_arguments)]
    fn run_pass_to_rt(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        pass_idx: usize,
        ubo: &wgpu::Buffer,
        main_view: &wgpu::TextureView,
        prev_analysis_view: &wgpu::TextureView,
        prev_depth_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
        history_view: &wgpu::TextureView,
        flow_view: &wgpu::TextureView,
        mesh_coord_view: &wgpu::TextureView,
        prev_mesh_coord_view: &wgpu::TextureView,
        semantic_view: &wgpu::TextureView,
        surface_cache_view: &wgpu::TextureView,
        prev_surface_cache_view: &wgpu::TextureView,
        subject_mask_view: &wgpu::TextureView,
        target: &wgpu::TextureView,
        w: u32,
        h: u32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        let bg = self.make_bind_group(device, ubo,
            main_view, prev_analysis_view, prev_depth_view, depth_view,
            history_view, flow_view, mesh_coord_view, prev_mesh_coord_view,
            semantic_view, surface_cache_view, prev_surface_cache_view, subject_mask_view,
        );
        self.run_pass(encoder, &self.pipelines[pass_idx], &bg, target, w, h, profiler);
    }

    // Build a bind group from 1 UBO + 12 texture views + 1 sampler.
    #[allow(clippy::too_many_arguments)]
    fn make_bind_group(
        &self,
        device: &wgpu::Device,
        ubo: &wgpu::Buffer,
        main_view: &wgpu::TextureView,
        prev_analysis_view: &wgpu::TextureView,
        prev_depth_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
        history_view: &wgpu::TextureView,
        flow_view: &wgpu::TextureView,
        mesh_coord_view: &wgpu::TextureView,
        prev_mesh_coord_view: &wgpu::TextureView,
        semantic_view: &wgpu::TextureView,
        surface_cache_view: &wgpu::TextureView,
        prev_surface_cache_view: &wgpu::TextureView,
        subject_mask_view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0,  resource: ubo.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1,  resource: wgpu::BindingResource::TextureView(main_view) },
                wgpu::BindGroupEntry { binding: 2,  resource: wgpu::BindingResource::TextureView(prev_analysis_view) },
                wgpu::BindGroupEntry { binding: 3,  resource: wgpu::BindingResource::TextureView(prev_depth_view) },
                wgpu::BindGroupEntry { binding: 4,  resource: wgpu::BindingResource::TextureView(depth_view) },
                wgpu::BindGroupEntry { binding: 5,  resource: wgpu::BindingResource::TextureView(history_view) },
                wgpu::BindGroupEntry { binding: 6,  resource: wgpu::BindingResource::TextureView(flow_view) },
                wgpu::BindGroupEntry { binding: 7,  resource: wgpu::BindingResource::TextureView(mesh_coord_view) },
                wgpu::BindGroupEntry { binding: 8,  resource: wgpu::BindingResource::TextureView(prev_mesh_coord_view) },
                wgpu::BindGroupEntry { binding: 9,  resource: wgpu::BindingResource::TextureView(semantic_view) },
                wgpu::BindGroupEntry { binding: 10, resource: wgpu::BindingResource::TextureView(surface_cache_view) },
                wgpu::BindGroupEntry { binding: 11, resource: wgpu::BindingResource::TextureView(prev_surface_cache_view) },
                wgpu::BindGroupEntry { binding: 12, resource: wgpu::BindingResource::TextureView(subject_mask_view) },
                wgpu::BindGroupEntry { binding: 13, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }

    // WireframeDepthFX.cs line 538-554 — UploadDnnDepthTexture
    fn upload_dnn_depth_texture(queue: &wgpu::Queue, state: &mut OwnerState) {
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
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &state.dnn_depth_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(state.analysis_width * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: state.analysis_width,
                height: state.analysis_height,
                depth_or_array_layers: 1,
            },
        );
        state.dnn_depth_dirty = false;
    }

    // WireframeDepthFX.cs line 556-572 — UploadDnnSubjectTexture
    fn upload_dnn_subject_texture(queue: &wgpu::Queue, state: &mut OwnerState) {
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
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &state.dnn_subject_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(state.analysis_width * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: state.analysis_width,
                height: state.analysis_height,
                depth_or_array_layers: 1,
            },
        );
        state.dnn_subject_dirty = false;
    }

    // WireframeDepthFX.cs line 574-594 — UploadNativeFlowTexture
    // nativeFlowPixels is Color (RGBAFloat) → upload as Rgba16Float (Metal: Rgba32Float not filterable)
    fn upload_native_flow_texture(queue: &wgpu::Queue, state: &mut OwnerState) {
        if !state.native_flow_dirty {
            return;
        }
        let count = (state.analysis_width * state.analysis_height) as usize;
        // Convert f32 flow data → f16 for Rgba16Float upload
        let floats = &state.native_flow_buffer[..count * 4];
        let mut f16_bytes: Vec<u8> = Vec::with_capacity(count * 8); // 4 halfs × 2 bytes
        for &f in floats {
            f16_bytes.extend_from_slice(&f32_to_f16(f).to_le_bytes());
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &state.native_flow_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &f16_bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(state.analysis_width * 8), // 4 halfs × 2 bytes
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: state.analysis_width,
                height: state.analysis_height,
                depth_or_array_layers: 1,
            },
        );
        state.native_flow_dirty = false;
    }

    /// Try to spawn 3 parallel workers (depth, flow, subject).
    /// Returns None if the plugin doesn't support specialized creation.
    fn try_spawn_parallel_workers() -> Option<WorkerMode> {
        let depth_worker = BackgroundWorker::try_new(|| {
            use manifold_native::depth_estimator::DepthEstimator;
            let mut estimator = manifold_native::ffi::depth_ffi::FfiDepthEstimator::new_depth_only()?;
            Some(move |req: DepthOnlyRequest| -> DepthOnlyResponse {
                let pc = (req.width * req.height) as usize;
                let mut depth = vec![0f32; pc];
                let ok = estimator.process(&req.pixel_data, req.width, req.height, &mut depth, req.width, req.height);
                DepthOnlyResponse { depth_buffer: if ok != 0 { Some(depth) } else { None } }
            })
        })?;

        let flow_worker = BackgroundWorker::try_new(|| {
            use manifold_native::depth_estimator::DepthEstimator;
            let mut estimator = manifold_native::ffi::depth_ffi::FfiDepthEstimator::new_flow_only()?;
            Some(move |req: FlowOnlyRequest| -> FlowOnlyResponse {
                if !req.has_prev_frame {
                    return FlowOnlyResponse { flow_buffer: None, cut_score: 0.0 };
                }
                let pc = (req.width * req.height) as usize;
                let mut flow = vec![0f32; pc * 4];
                let mut cut = vec![0f32; 1];
                let ok = estimator.compute_flow(
                    &req.prev_pixel_data, &req.pixel_data,
                    req.width, req.height, &mut flow, req.width, req.height, &mut cut,
                );
                if ok != 0 { FlowOnlyResponse { flow_buffer: Some(flow), cut_score: cut[0] } }
                else { FlowOnlyResponse { flow_buffer: None, cut_score: 0.0 } }
            })
        })?;

        let subject_worker = BackgroundWorker::try_new(|| {
            use manifold_native::depth_estimator::DepthEstimator;
            let mut estimator = manifold_native::ffi::depth_ffi::FfiDepthEstimator::new_subject_only()?;
            Some(move |req: SubjectOnlyRequest| -> SubjectOnlyResponse {
                let pc = (req.width * req.height) as usize;
                let mut mask = vec![0f32; pc];
                let ok = estimator.process_subject_mask(&req.pixel_data, req.width, req.height, &mut mask, req.width, req.height);
                if ok != 0 {
                    const BLEND: f32 = 0.55;
                    let blended: Vec<f32> = if req.has_subject_mask_history {
                        let mut hist = req.subject_history;
                        for i in 0..pc { hist[i] = hist[i] + (mask[i].clamp(0.0, 1.0) - hist[i]) * BLEND; }
                        hist
                    } else {
                        mask.iter().map(|v| v.clamp(0.0, 1.0)).collect()
                    };
                    SubjectOnlyResponse { subject_history_blended: Some(blended), subject_api_failed: false }
                } else {
                    SubjectOnlyResponse { subject_history_blended: None, subject_api_failed: true }
                }
            })
        })?;

        Some(WorkerMode::Parallel { depth_worker, flow_worker, subject_worker })
    }

    /// Try to spawn workers: parallel mode first, monolithic fallback.
    fn try_spawn_workers() -> Option<WorkerMode> {
        if let Some(parallel) = Self::try_spawn_parallel_workers() {
            log::info!("[WireframeDepthFX] Spawned 3 parallel native workers");
            return Some(parallel);
        }
        let worker = Self::try_spawn_monolithic_worker()?;
        log::info!("[WireframeDepthFX] Parallel spawn failed; falling back to monolithic worker");
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
                        &req.prev_pixel_data, &req.pixel_data,
                        w, h, &mut flow, w, h, &mut cut,
                    );
                    if ok != 0 { (Some(flow), cut[0]) } else { (None, 0.0) }
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
                        (None, true) // API not available in this plugin build
                    }
                } else {
                    (None, false)
                };

                DepthResponse { flow_buffer, cut_score, depth_buffer, subject_history_blended, subject_api_failed }
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
        if self.dnn_backend_initialized && !self.dnn_backend_available
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

    // WireframeDepthFX.cs line 715-728 — DisableDnnBackend
    #[allow(dead_code)]
    fn disable_dnn_backend(&mut self, frame_count: i64) {
        self.workers = None;
        self.dnn_backend_initialized = true;
        self.dnn_backend_available = false;
        self.dnn_next_retry_frame = frame_count + 300;
    }

    // WireframeDepthFX.cs line 455-495 — RequestNativeReadback
    fn request_native_readback(
        &mut self,
        gpu: &mut GpuEncoder,
        _source: &wgpu::TextureView,
        source_tex: &wgpu::Texture,
        owner_key: i64,
        mode: DepthSourceMode,
        subject_isolation: f32,
        frame_count: i64,
    ) {
        let state = match self.owner_states.get_mut(&owner_key) {
            Some(s) => s,
            None => return,
        };

        // WireframeDepthFX.cs line 465-472
        let wants_depth = mode == DepthSourceMode::Dnn;
        let wants_flow = true;
        let wants_subject =
            self.dnn_subject_api_available
            && mode == DepthSourceMode::Dnn
            && subject_isolation > 0.02
            && frame_count - state.last_subject_request_frame >= NATIVE_UPDATE_INTERVAL_SUBJECT;
        if !wants_depth && !wants_flow && !wants_subject {
            return;
        }

        // WireframeDepthFX.cs line 475-478
        let min_interval = if mode == DepthSourceMode::Dnn {
            NATIVE_UPDATE_INTERVAL_DNN
        } else {
            NATIVE_UPDATE_INTERVAL_HEURISTIC
        };
        if frame_count - state.last_native_request_frame < min_interval {
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
        // Copy source → dnn_input_tex via a blit (we copy at texture level since both are Rgba8Unorm)
        let copy_aw = state.analysis_width;
        let copy_ah = state.analysis_height;
        Self::encode_copy(gpu, source_tex, &state.dnn_input_tex.texture, copy_aw, copy_ah);

        state.native_request_wants_depth   = wants_depth;
        state.native_request_wants_flow    = wants_flow;
        state.native_request_wants_subject = wants_subject;
        state.last_native_request_frame    = frame_count;
        if wants_subject {
            state.last_subject_request_frame = frame_count;
        }

        let aw = state.analysis_width;
        let ah = state.analysis_height;
        // Readback via gpu.encoder (wgpu aux encoder in hal mode) — map_async
        // needs wgpu tracking to fire the callback.
        state.readback.submit(gpu.device, gpu.encoder, &state.dnn_input_tex.texture, aw, ah);
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
            state.native_flow_dirty    = true;
            state.native_flow_ready    = true;
            state.latest_cut_score     = response.cut_score;
        } else {
            state.native_flow_has_data = false;
            state.native_flow_ready    = false;
            state.latest_cut_score     = 0.0;
        }

        // Depth
        if let Some(ref depth) = response.depth_buffer {
            let copy_len = depth.len().min(state.dnn_depth_buffer.len());
            state.dnn_depth_buffer[..copy_len].copy_from_slice(&depth[..copy_len]);
            state.dnn_has_depth   = true;
            state.dnn_depth_dirty = true;
        }

        // Subject mask (temporally blended on worker thread)
        if let Some(ref blended) = response.subject_history_blended {
            let copy_len = blended.len().min(state.dnn_subject_history_buffer.len());
            state.dnn_subject_history_buffer[..copy_len].copy_from_slice(&blended[..copy_len]);
            state.dnn_has_subject_mask = true;
            state.dnn_subject_dirty    = true;
        }
    }

    // WireframeDepthFX.cs line 894-913 — EstimateDepthHeuristic
    fn estimate_depth_heuristic(
        &self,
        gpu: &mut GpuEncoder,
        analysis_view: &wgpu::TextureView,
        state: &mut OwnerState,
        _temporal_smooth: f32,
        uniforms: &WireUniforms,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        let aw = state.analysis_width;
        let ah = state.analysis_height;
        let depth_next = RenderTarget::new(gpu.device, aw, ah, wgpu::TextureFormat::Rgba16Float, "WD DepthNext");

        // PASS_HEURISTIC_DEPTH: analysis → depthNext
        self.encode_pass(gpu, PASS_HEURISTIC_DEPTH, uniforms,
            analysis_view, &state.previous_analysis_tex.view, &state.depth_tex.view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &depth_next.view, aw, ah, profiler);

        // Graphics.Blit(depthNext, state.depthTex) — copy
        Self::encode_copy(gpu, &depth_next.texture, &state.depth_tex.texture, aw, ah);
    }

    // WireframeDepthFX.cs line 420-453 — TryEstimateDepthDnn
    fn try_estimate_depth_dnn(
        &self,
        gpu: &mut GpuEncoder,
        state: &mut OwnerState,
        _temporal_smooth: f32,
        uniforms: &WireUniforms,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) -> bool {
        // dnnBackendAvailable checked by caller (ensure_dnn_backend_available)
        if state.dnn_depth_dirty {
            Self::upload_dnn_depth_texture(gpu.queue, state);
        }
        if !state.dnn_has_depth {
            return false;
        }

        let aw = state.analysis_width;
        let ah = state.analysis_height;
        let depth_next = RenderTarget::new(gpu.device, aw, ah, wgpu::TextureFormat::Rgba16Float, "WD DnnDepthNext");

        // PASS_DNN_DEPTH_POST: dnnDepthTexture → depthNext
        self.encode_pass(gpu, PASS_DNN_DEPTH_POST, uniforms,
            &state.dnn_depth_texture_view, &self.dummy_view, &state.depth_tex.view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &depth_next.view, aw, ah, profiler);

        // Graphics.Blit(depthNext, state.depthTex)
        Self::encode_copy(gpu, &depth_next.texture, &state.depth_tex.texture, aw, ah);

        true
    }

    // WireframeDepthFX.cs line 730-892 — UpdateFlowLock
    fn update_flow_lock(
        &self,
        gpu: &mut GpuEncoder,
        analysis_view: &wgpu::TextureView,
        state: &mut OwnerState,
        _temporal_smooth: f32,
        mesh_rate: i32,
        native_flow_enabled: bool,
        face_warp_enabled: bool,
        frame_count: i64,
        uniforms: &WireUniforms,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        // WireframeDepthFX.cs line 738-740
        // (null checks — all fields valid if we reached here)

        // WireframeDepthFX.cs line 742-743
        if native_flow_enabled && state.native_flow_dirty {
            Self::upload_native_flow_texture(gpu.queue, state);
        }

        // WireframeDepthFX.cs line 747-748
        let use_native_flow = native_flow_enabled
            && state.native_flow_has_data;

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
            self.encode_pass(gpu, PASS_INIT_MESH_COORD, &cut_uniforms,
                &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
                &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
                &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
                &state.mesh_coord_tex.view, aw, ah, profiler);
            self.encode_pass(gpu, PASS_SURFACE_CACHE_UPDATE, &cut_uniforms,
                &state.mesh_coord_tex.view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
                &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
                &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
                &state.surface_cache_tex.view, aw, ah, profiler);

            state.dnn_has_subject_mask = false;
            if !state.dnn_subject_history_buffer.is_empty() {
                state.dnn_subject_history_buffer.fill(0.0);
            }
            state.latest_cut_score   = 0.0;
            state.native_flow_ready  = false;
            state.native_flow_has_data = false;
            state.last_mesh_update_frame = frame_count;
            // Blit analysis → previousAnalysisTex
            Self::encode_copy(gpu, &state.dnn_input_tex.texture, &state.previous_analysis_tex.texture, aw, ah);
            return;
        }

        // WireframeDepthFX.cs line 770-776 — amortization check
        let run_mesh_pipeline = mesh_rate <= 1
            || frame_count - state.last_mesh_update_frame >= mesh_rate as i64;
        if !run_mesh_pipeline {
            return;
        }
        state.last_mesh_update_frame = frame_count;

        let aw = state.analysis_width;
        let ah = state.analysis_height;

        // WireframeDepthFX.cs line 779-789 — choose flow source
        let flow_input_view: &wgpu::TextureView = if use_native_flow {
            &state.native_flow_texture_view
        } else {
            // PASS_FLOW_ESTIMATE: analysis → flowTex
            self.encode_pass(gpu, PASS_FLOW_ESTIMATE, uniforms,
                analysis_view, &state.previous_analysis_tex.view,
                &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
                &self.dummy_view, &self.dummy_view, &self.dummy_view,
                &self.dummy_view, &self.dummy_view, &self.dummy_view,
                &state.flow_tex.view, aw, ah, profiler);
            &state.flow_tex.view
        };

        // WireframeDepthFX.cs line 792-826 — flowFiltered, temp RTs
        let flow_filtered = RenderTarget::new(gpu.device, aw, ah, wgpu::TextureFormat::Rgba16Float, "WD FlowFiltered");
        // PASS_FLOW_HYGIENE: flowInput → flowFiltered
        self.encode_pass(gpu, PASS_FLOW_HYGIENE, uniforms,
            flow_input_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &flow_filtered.view, aw, ah, profiler);
        let flow_stable_view = &flow_filtered.view;

        // WireframeDepthFX.cs line 808-826: semantic mask
        // PASS_SEMANTIC_MASK: analysis → semanticTex
        self.encode_pass(gpu, PASS_SEMANTIC_MASK, uniforms,
            analysis_view, &state.previous_analysis_tex.view,
            &self.dummy_view, &state.depth_tex.view,
            &self.dummy_view, flow_stable_view,
            &self.dummy_view, &self.dummy_view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &state.semantic_tex.view, aw, ah, profiler);

        // WireframeDepthFX.cs line 811-826: temp coord RTs
        let coord_next       = RenderTarget::new(gpu.device, aw, ah, wgpu::TextureFormat::Rgba16Float, "WD CoordNext");
        let coord_affine     = RenderTarget::new(gpu.device, aw, ah, wgpu::TextureFormat::Rgba16Float, "WD CoordAffine");
        let coord_regularized = RenderTarget::new(gpu.device, aw, ah, wgpu::TextureFormat::Rgba16Float, "WD CoordReg");
        let surface_next     = RenderTarget::new(gpu.device, aw, ah, wgpu::TextureFormat::Rgba16Float, "WD SurfaceNext");

        // WireframeDepthFX.cs line 829-835: PASS_FLOW_ADVECT_COORD
        self.encode_pass(gpu, PASS_FLOW_ADVECT_COORD, uniforms,
            analysis_view, &state.previous_analysis_tex.view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view,
            flow_stable_view, &self.dummy_view, &state.mesh_coord_tex.view,
            &state.semantic_tex.view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &coord_next.view, aw, ah, profiler);

        // WireframeDepthFX.cs line 837-841: PASS_MESH_CELL_AFFINE
        self.encode_pass(gpu, PASS_MESH_CELL_AFFINE, uniforms,
            &coord_next.view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, flow_stable_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &coord_affine.view, aw, ah, profiler);

        // WireframeDepthFX.cs line 843-862: optional face warp pass
        let pre_regularize_view: &wgpu::TextureView;
        let coord_face_opt: Option<RenderTarget>;
        if face_warp_enabled {
            let coord_face = RenderTarget::new(gpu.device, aw, ah, wgpu::TextureFormat::Rgba16Float, "WD CoordFace");
            let edge_follow_mask_view = if state.dnn_has_subject_mask {
                &state.dnn_subject_texture_view
            } else {
                &state.semantic_tex.view
            };
            self.encode_pass(gpu, PASS_MESH_FACE_WARP, uniforms,
                &coord_affine.view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
                &self.dummy_view, flow_stable_view, &self.dummy_view, &self.dummy_view,
                edge_follow_mask_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
                &coord_face.view, aw, ah, profiler);
            coord_face_opt = Some(coord_face);
            pre_regularize_view = &coord_face_opt.as_ref().unwrap().view;
        } else {
            coord_face_opt = None;
            pre_regularize_view = &coord_affine.view;
        }

        // WireframeDepthFX.cs line 863-871: PASS_MESH_REGULARIZE
        self.encode_pass(gpu, PASS_MESH_REGULARIZE, uniforms,
            pre_regularize_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, flow_stable_view, &self.dummy_view,
            &state.mesh_coord_tex.view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &coord_regularized.view, aw, ah, profiler);
        Self::encode_copy(gpu, &coord_regularized.texture, &state.mesh_coord_tex.texture, aw, ah);

        // WireframeDepthFX.cs line 873-879: PASS_SURFACE_CACHE_UPDATE
        self.encode_pass(gpu, PASS_SURFACE_CACHE_UPDATE, uniforms,
            &state.mesh_coord_tex.view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, flow_stable_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, &self.dummy_view, &state.surface_cache_tex.view, &self.dummy_view,
            &surface_next.view, aw, ah, profiler);
        Self::encode_copy(gpu, &surface_next.texture, &state.surface_cache_tex.texture, aw, ah);
        // coord_face_opt drops here (ReleaseTemporary equivalent)
        let _ = coord_face_opt;
    }

    // WireframeDepthFX.cs line 927-978 — ClearOwnerState
    #[allow(dead_code)]
    fn clear_owner_state(gpu: &mut GpuEncoder, state: &mut OwnerState) {
        Self::encode_clear(gpu, &state.previous_analysis_tex);
        Self::encode_clear(gpu, &state.depth_tex);
        Self::encode_clear(gpu, &state.line_history_tex);
        Self::encode_clear(gpu, &state.flow_tex);
        Self::encode_clear(gpu, &state.mesh_coord_tex);
        Self::encode_clear(gpu, &state.semantic_tex);
        Self::encode_clear(gpu, &state.surface_cache_tex);
        state.dnn_readback_pending     = false;
        state.dnn_has_depth            = false;
        state.dnn_depth_dirty          = false;
        state.dnn_has_subject_mask     = false;
        state.dnn_subject_dirty        = false;
        state.has_prev_native_frame    = false;
        state.native_flow_has_data     = false;
        state.native_flow_dirty        = false;
        state.native_flow_ready        = false;
        state.native_request_wants_flow    = false;
        state.native_request_wants_depth   = false;
        state.native_request_wants_subject = false;
        state.latest_cut_score         = 0.0;
        state.last_subject_request_frame   = -1024;
        state.last_mesh_update_frame       = -1024;
        // Clear CPU pixel buffers (equivalent to SetPixels32 with zeros)
        state.dnn_depth_buffer.fill(0.0);
        state.dnn_subject_history_buffer.fill(0.0);
        state.native_flow_buffer.fill(0.0);
    }
}

impl PostProcessEffect for WireframeDepthFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::WIREFRAME_DEPTH
    }

    fn supports_hal(&self) -> bool { true }

    // WireframeDepthFX.cs line 279-361 — Apply
    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        _target_texture: &wgpu::Texture,
        fx: &EffectInstance,
        ctx: &EffectContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        // WireframeDepthFX.cs line 281-282
        let amount = fx.param_values.first().copied().unwrap_or(0.0);
        if amount <= 0.0 {
            return;
        }

        // Read params — new 12-param layout (see effect_definition_registry.rs)
        let wire_scale     = fx.param_values.get(7).copied().unwrap_or(1.0).clamp(0.5, 1.0);
        let mesh_rate      = fx.param_values.get(8).copied().unwrap_or(1.0).round() as i32;
        let mesh_rate      = mesh_rate.clamp(1, 4);
        let native_flow_enabled = fx.param_values.get(9).copied().unwrap_or(0.0).round() as i32 > 0;
        let flow_lock_enabled   = fx.param_values.get(10).copied().unwrap_or(0.0).round() as i32 > 0;
        let face_warp_enabled   = fx.param_values.get(11).copied().unwrap_or(0.0) > 0.01;

        // GetOrCreateOwner needs encoder; owner_states borrow released before later use.
        // We store the owner_key to look up the state again after this call.
        let owner_key = ctx.owner_key;
        self.get_or_create_owner(gpu, owner_key, wire_scale, profiler);

        // Read remaining params — new 12-param layout
        let density         = fx.param_values.get(1).copied().unwrap_or(96.0);
        let line_width      = fx.param_values.get(2).copied().unwrap_or(1.2);
        let depth_scale     = fx.param_values.get(3).copied().unwrap_or(1.0);
        let temporal_smooth = fx.param_values.get(4).copied().unwrap_or(0.8);
        let persistence     = 0.82; // hardcoded default (Persist param removed from UI)
        let depth_mode      = DepthSourceMode::Dnn; // always DNN (Depth param removed from UI)
        let subject_isolation = fx.param_values.get(5).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        let blend_mode        = fx.param_values.get(6).copied().unwrap_or(0.0).clamp(0.0, 6.0);

        // ── Poll background worker(s) for completed native results ──
        match &mut self.workers {
            Some(WorkerMode::Parallel { depth_worker, flow_worker, subject_worker }) => {
                if let Some(resp) = depth_worker.try_recv() {
                    let ok = self.pending_depth_owner.take().unwrap_or(owner_key);
                    if let Some(state) = self.owner_states.get_mut(&ok)
                        && let Some(ref depth) = resp.depth_buffer {
                            let copy_len = depth.len().min(state.dnn_depth_buffer.len());
                            state.dnn_depth_buffer[..copy_len].copy_from_slice(&depth[..copy_len]);
                            state.dnn_has_depth = true;
                            state.dnn_depth_dirty = true;
                        }
                }
                if let Some(resp) = flow_worker.try_recv() {
                    let ok = self.pending_flow_owner.take().unwrap_or(owner_key);
                    if let Some(state) = self.owner_states.get_mut(&ok) {
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
                }
                if let Some(resp) = subject_worker.try_recv() {
                    if resp.subject_api_failed {
                        self.dnn_subject_api_available = false;
                    }
                    let ok = self.pending_subject_owner.take().unwrap_or(owner_key);
                    if let Some(state) = self.owner_states.get_mut(&ok)
                        && let Some(ref blended) = resp.subject_history_blended {
                            let copy_len = blended.len().min(state.dnn_subject_history_buffer.len());
                            state.dnn_subject_history_buffer[..copy_len].copy_from_slice(&blended[..copy_len]);
                            state.dnn_has_subject_mask = true;
                            state.dnn_subject_dirty = true;
                        }
                }
            }
            Some(WorkerMode::Monolithic { worker }) => {
                if let Some(response) = worker.try_recv() {
                    let result_owner = self.pending_depth_owner.take().unwrap_or(owner_key);
                    if let Some(state) = self.owner_states.get_mut(&result_owner) {
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
        if let Some(state) = self.owner_states.get_mut(&owner_key)
            && state.dnn_readback_pending
                && let Some(pixels) = state.readback.try_read(gpu.device) {
                    state.dnn_readback_pending = false;
                    let aw = state.analysis_width as i32;
                    let ah = state.analysis_height as i32;

                    match &mut self.workers {
                        Some(WorkerMode::Parallel { depth_worker, flow_worker, subject_worker }) => {
                            if state.native_request_wants_depth {
                                depth_worker.submit(DepthOnlyRequest {
                                    pixel_data: pixels.clone(), width: aw, height: ah,
                                });
                                self.pending_depth_owner = Some(owner_key);
                            }
                            if state.native_request_wants_flow {
                                flow_worker.submit(FlowOnlyRequest {
                                    pixel_data: pixels.clone(),
                                    prev_pixel_data: state.prev_native_pixel_buffer.clone(),
                                    has_prev_frame: state.has_prev_native_frame,
                                    width: aw, height: ah,
                                });
                                self.pending_flow_owner = Some(owner_key);
                            }
                            if state.native_request_wants_subject {
                                subject_worker.submit(SubjectOnlyRequest {
                                    pixel_data: pixels.clone(),
                                    width: aw, height: ah,
                                    has_subject_mask_history: state.dnn_has_subject_mask,
                                    subject_history: state.dnn_subject_history_buffer.clone(),
                                });
                                self.pending_subject_owner = Some(owner_key);
                            }
                        }
                        Some(WorkerMode::Monolithic { worker }) => {
                            let req = DepthRequest {
                                pixel_data: pixels.clone(),
                                prev_pixel_data: state.prev_native_pixel_buffer.clone(),
                                has_prev_frame: state.has_prev_native_frame,
                                width: aw, height: ah,
                                wants_flow: state.native_request_wants_flow,
                                wants_depth: state.native_request_wants_depth,
                                wants_subject: state.native_request_wants_subject,
                                has_subject_mask_history: state.dnn_has_subject_mask,
                                subject_history: state.dnn_subject_history_buffer.clone(),
                            };
                            worker.submit(req);
                            self.pending_depth_owner = Some(owner_key);
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
        // WireframeDepthFX.cs line 829: flowLockStrength = Lerp(0.76, 0.985, Clamp01(temporalSmooth))
        let ts01 = temporal_smooth.clamp(0.0, 1.0);
        let flow_lock_strength  = 0.76 + (0.985 - 0.76) * ts01;
        // WireframeDepthFX.cs line 838: cellAffine = Lerp(0.40, 0.88, ...)
        let cell_affine         = 0.40 + (0.88 - 0.40) * ts01;
        // EdgeFollow (param 11) scales the face warp strength. At 1.0 = original behavior.
        let edge_follow = fx.param_values.get(11).copied().unwrap_or(0.5).clamp(0.0, 1.0);
        let face_warp_strength  = (0.25 + (0.90 - 0.25) * ts01) * edge_follow;
        // WireframeDepthFX.cs line 864: regularize = Lerp(0.40, 0.74, ...)
        let mesh_regularize     = 0.40 + (0.74 - 0.40) * ts01;
        // WireframeDepthFX.cs line 874: surfacePersist = Lerp(0.80, 0.985, ...)
        let surface_persistence = 0.80 + (0.985 - 0.80) * ts01;
        // WireframeDepthFX.cs line 334: wireTaa = Lerp(0.48, 0.92, Clamp01(temporalSmooth))
        let wire_taa            = 0.48 + (0.92 - 0.48) * ts01;

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

        // Reset ring buffer slot counter at frame start (hal path).
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        self.hal_ring_offset.set(0);

        // --- EstimateDepth ---
        // WireframeDepthFX.cs line 363-418

        // PASS_ANALYSIS: source → analysis (temp RT at analysis resolution)
        let analysis_rt = RenderTarget::new(gpu.device, aw, ah, wgpu::TextureFormat::Rgba8Unorm, "WD Analysis");
        self.encode_pass(gpu, PASS_ANALYSIS, &uniforms_analysis,
            source, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &self.dummy_view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
            &analysis_rt.view, aw, ah, profiler);

        // WireframeDepthFX.cs line 388-394 — request native readback
        if native_flow_enabled && flow_lock_enabled {
            let mesh_update_due = {
                let state = self.owner_states.get(&owner_key).unwrap();
                mesh_rate <= 1
                    || ctx.frame_count - state.last_mesh_update_frame >= mesh_rate as i64
            };
            if mesh_update_due {
                // Copy analysis_rt → dnn_input_tex happens inside request_native_readback via encoder copy
                self.request_native_readback(
                    gpu,
                    &analysis_rt.view, &analysis_rt.texture,
                    owner_key, depth_mode, subject_isolation, ctx.frame_count,
                );
            }
        }

        // WireframeDepthFX.cs line 396-407 — depth estimation
        // Temporarily remove state to avoid borrow conflict (self.method + self.owner_states)
        let dnn_used = if depth_mode == DepthSourceMode::Dnn && dnn_available {
            let mut state = self.owner_states.remove(&owner_key).unwrap();
            let result = self.try_estimate_depth_dnn(gpu, &mut state, temporal_smooth, &uniforms_analysis, profiler);
            self.owner_states.insert(owner_key, state);
            result
        } else {
            false
        };
        if !dnn_used {
            if depth_mode == DepthSourceMode::Dnn && !self.warned_missing_dnn && !self.dnn_backend_available {
                log::warn!("[WireframeDepthFX] DNN depth path requested, but no backend is configured. \
                           Falling back to heuristic depth.");
                self.warned_missing_dnn = true;
            }
            let mut state = self.owner_states.remove(&owner_key).unwrap();
            self.estimate_depth_heuristic(gpu, &analysis_rt.view, &mut state, temporal_smooth, &uniforms_analysis, profiler);
            self.owner_states.insert(owner_key, state);
        }

        // WireframeDepthFX.cs line 409-412 — UpdateFlowLock or blit analysis → previousAnalysisTex
        if flow_lock_enabled {
            let mut state = self.owner_states.remove(&owner_key).unwrap();
            self.update_flow_lock(
                gpu,
                &analysis_rt.view, &mut state,
                temporal_smooth, mesh_rate, native_flow_enabled, face_warp_enabled,
                ctx.frame_count, &uniforms_analysis,
                profiler,
            );
            self.owner_states.insert(owner_key, state);
        }

        // Always copy analysis → previousAnalysisTex (WireframeDepthFX.cs line 412 / 891)
        {
            let state = self.owner_states.get(&owner_key).unwrap();
            Self::encode_copy(gpu, &analysis_rt.texture, &state.previous_analysis_tex.texture, aw, ah);
        }

        // --- Upload DNN subject texture if dirty ---
        // WireframeDepthFX.cs line 311-312
        {
            let state = self.owner_states.get_mut(&owner_key).unwrap();
            if state.dnn_subject_dirty {
                Self::upload_dnn_subject_texture(gpu.queue, state);
            }
        }

        // --- Wire mask pass (Pass 2) ---
        // WireframeDepthFX.cs line 305-328
        let line_mask = RenderTarget::new(gpu.device, ww, wh, wgpu::TextureFormat::Rgba8Unorm, "WD LineMask");
        {
            let state = self.owner_states.get(&owner_key).unwrap();
            let subject_mask_view = if depth_mode == DepthSourceMode::Dnn
                && state.dnn_has_subject_mask
            {
                &state.dnn_subject_texture_view
            } else {
                &self.dummy_view
            };
            self.encode_pass(gpu, PASS_WIREFRAME_MASK, &uniforms_wire,
                source, &self.dummy_view, &self.dummy_view,
                &state.depth_tex.view, &self.dummy_view, &self.dummy_view,
                &state.mesh_coord_tex.view, &self.dummy_view,
                &state.semantic_tex.view, &state.surface_cache_tex.view,
                &self.dummy_view, subject_mask_view,
                &line_mask.view, ww, wh, profiler);
        }

        // --- Update history pass (Pass 3) + copy → lineHistoryTex ---
        // WireframeDepthFX.cs line 330-347
        let history_next = RenderTarget::new(gpu.device, ww, wh, wgpu::TextureFormat::Rgba8Unorm, "WD HistoryNext");
        {
            let state = self.owner_states.get(&owner_key).unwrap();
            self.encode_pass(gpu, PASS_UPDATE_HISTORY, &uniforms_wire,
                &line_mask.view, &self.dummy_view, &self.dummy_view, &self.dummy_view,
                &state.line_history_tex.view, &self.dummy_view,
                &self.dummy_view, &self.dummy_view, &self.dummy_view,
                &state.surface_cache_tex.view, &self.dummy_view, &self.dummy_view,
                &history_next.view, ww, wh, profiler);
            Self::encode_copy(gpu, &history_next.texture, &state.line_history_tex.texture, ww, wh);
        }

        // --- Composite pass (Pass 4) → target ---
        // WireframeDepthFX.cs line 349-355
        {
            let state = self.owner_states.get(&owner_key).unwrap();
            self.encode_pass(gpu, PASS_COMPOSITE, &uniforms_source,
                source, &self.dummy_view, &self.dummy_view, &self.dummy_view,
                &state.line_history_tex.view, &self.dummy_view,
                &self.dummy_view, &self.dummy_view, &self.dummy_view,
                &self.dummy_view, &self.dummy_view, &self.dummy_view,
                target, self.width, self.height, profiler);
        }
    }

    // WireframeDepthFX.cs line 915-919 — ClearState (all owners)
    fn clear_state(&mut self) {
        // We can't call encoder from trait — clear flags + CPU buffers, GPU cleared on next apply.
        for state in self.owner_states.values_mut() {
            state.dnn_readback_pending     = false;
            state.dnn_has_depth            = false;
            state.dnn_depth_dirty          = false;
            state.dnn_has_subject_mask     = false;
            state.dnn_subject_dirty        = false;
            state.has_prev_native_frame    = false;
            state.native_flow_has_data     = false;
            state.native_flow_dirty        = false;
            state.native_flow_ready        = false;
            state.native_request_wants_flow    = false;
            state.native_request_wants_depth   = false;
            state.native_request_wants_subject = false;
            state.latest_cut_score         = 0.0;
            state.last_subject_request_frame   = -1024;
            state.last_mesh_update_frame       = -1024;
            state.dnn_depth_buffer.fill(0.0);
            state.dnn_subject_history_buffer.fill(0.0);
            state.native_flow_buffer.fill(0.0);
        }
    }

    fn resize(&mut self, _device: &wgpu::Device, width: u32, height: u32) {
        // WireframeDepthFX.cs line 133-137 — InitializeState
        self.width  = width;
        self.height = height;
        // Per-owner textures are rebuilt lazily in GetOrCreateOwner.
        self.owner_states.clear();
    }

    fn cleanup_owner_state(&mut self, owner_key: i64) {
        self.owner_states.remove(&owner_key);
    }
}

impl StatefulEffect for WireframeDepthFX {
    // WireframeDepthFX.cs line 921-925 — ClearState(ownerKey)
    fn clear_state_for_owner(&mut self, owner_key: i64) {
        if let Some(state) = self.owner_states.get_mut(&owner_key) {
            state.dnn_readback_pending     = false;
            state.dnn_has_depth            = false;
            state.dnn_depth_dirty          = false;
            state.dnn_has_subject_mask     = false;
            state.dnn_subject_dirty        = false;
            state.has_prev_native_frame    = false;
            state.native_flow_has_data     = false;
            state.native_flow_dirty        = false;
            state.native_flow_ready        = false;
            state.native_request_wants_flow    = false;
            state.native_request_wants_depth   = false;
            state.native_request_wants_subject = false;
            state.latest_cut_score         = 0.0;
            state.last_subject_request_frame   = -1024;
            state.last_mesh_update_frame       = -1024;
            state.dnn_depth_buffer.fill(0.0);
            state.dnn_subject_history_buffer.fill(0.0);
            state.native_flow_buffer.fill(0.0);
        }
    }

    // WireframeDepthFX.cs line 981-988 — CleanupOwner
    fn cleanup_owner(&mut self, owner_key: i64) {
        self.owner_states.remove(&owner_key);
    }

    // WireframeDepthFX.cs line 990-996 — CleanupAllOwners
    fn cleanup_all_owners(&mut self, _device: &wgpu::Device) {
        self.owner_states.clear();
        self.warned_missing_dnn = false;
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
