// Mechanical port of Unity WireframeDepthFX.cs.
// Unity source: Assets/Scripts/Compositing/Effects/WireframeDepthFX.cs
//
// WireframeDepthFX : SimpleBlitEffect, IStatefulEffect
// 15 render passes, per-owner state with ~20 GPU textures + CPU buffers.
// DNN backend is optional (None → heuristic depth only).

use std::collections::HashMap;
use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::render_target::RenderTarget;
use crate::gpu_readback::ReadbackRequest;
use manifold_native::depth_estimator::DepthEstimator;

// WireframeDepthFX.cs line 21-35
const PASS_ANALYSIS:             usize = 0;
const PASS_HEURISTIC_DEPTH:      usize = 1;
const PASS_WIREFRAME_MASK:       usize = 2;
const PASS_UPDATE_HISTORY:       usize = 3;
const PASS_COMPOSITE:            usize = 4;
const PASS_DNN_DEPTH_POST:       usize = 5;
const PASS_FLOW_ESTIMATE:        usize = 6;
const PASS_FLOW_ADVECT_COORD:    usize = 7;
const PASS_INIT_MESH_COORD:      usize = 8;
const PASS_MESH_REGULARIZE:      usize = 9;
const PASS_MESH_CELL_AFFINE:     usize = 10;
const PASS_SEMANTIC_MASK:        usize = 11;
const PASS_MESH_FACE_WARP:       usize = 12;
const PASS_SURFACE_CACHE_UPDATE: usize = 13;
const PASS_FLOW_HYGIENE:         usize = 14;
const PASS_COUNT:                usize = 15;

// WireframeDepthFX.cs line 36-39
const MAX_ANALYSIS_DIM:              i32 = 360;
const NATIVE_UPDATE_INTERVAL_DNN:    i32 = 2;
const NATIVE_UPDATE_INTERVAL_HEURISTIC: i32 = 4;
const NATIVE_UPDATE_INTERVAL_SUBJECT:   i32 = 4;

// WireframeDepthFX.cs line 41-45
#[derive(Clone, Copy, PartialEq, Eq)]
enum DepthSourceMode {
    Heuristic = 0,
    Dnn       = 1,
}

// WireframeDepthFX.cs line 47-90 — OwnerState
struct OwnerState {
    analysis_width:  i32,
    analysis_height: i32,
    wire_width:      i32,
    wire_height:     i32,

    // Render textures (analysis size)
    previous_analysis_tex: RenderTarget, // ARGB32 → Rgba8Unorm
    depth_tex:             RenderTarget, // ARGBHalf → Rgba16Float
    flow_tex:              RenderTarget, // ARGBHalf → Rgba16Float
    mesh_coord_tex:        RenderTarget, // ARGBHalf → Rgba16Float
    semantic_tex:          RenderTarget, // ARGBHalf → Rgba16Float
    surface_cache_tex:     RenderTarget, // ARGBHalf → Rgba16Float
    dnn_input_tex:         RenderTarget, // ARGB32 → Rgba8Unorm

    // Wire-size render texture
    line_history_tex: RenderTarget,      // ARGB32 → Rgba8Unorm

    // DNN depth CPU state
    dnn_readback_pending: bool,
    dnn_has_depth:        bool,
    dnn_depth_dirty:      bool,
    dnn_pixel_buffer:     Vec<u8>,       // analysis_w * analysis_h * 4
    dnn_depth_buffer:     Vec<f32>,      // analysis_w * analysis_h
    dnn_depth_texture:    RenderTarget,  // RGBA32 → Rgba8Unorm (upload from CPU)

    // DNN subject mask CPU state
    dnn_has_subject_mask:      bool,
    dnn_subject_dirty:         bool,
    dnn_subject_buffer:        Vec<f32>, // analysis_w * analysis_h
    dnn_subject_history_buffer: Vec<f32>, // analysis_w * analysis_h
    dnn_subject_texture:       RenderTarget, // Rgba8Unorm

    // Native optical flow CPU state
    has_prev_native_frame:  bool,
    prev_native_pixel_buffer: Vec<u8>,  // analysis_w * analysis_h * 4
    native_flow_buffer:     Vec<f32>,   // analysis_w * analysis_h * 4
    native_flow_texture:    RenderTarget, // RGBAFloat → Rgba32Float
    native_flow_has_data:   bool,
    native_flow_dirty:      bool,
    native_flow_ready:      bool,
    cut_score_buffer:       Vec<f32>,   // len 1
    latest_cut_score:       f32,

    // Timing/throttle
    last_native_request_frame:  i32,
    last_subject_request_frame: i32,
    last_mesh_update_frame:     i32,

    // Request flags (set before readback, read in callback)
    native_request_wants_flow:    bool,
    native_request_wants_depth:   bool,
    native_request_wants_subject: bool,

    // GPU readback
    readback: ReadbackRequest,
}

impl OwnerState {
    fn new(
        device: &wgpu::Device,
        analysis_width: i32,
        analysis_height: i32,
        wire_width: i32,
        wire_height: i32,
        owner_key: i64,
    ) -> Self {
        let aw = analysis_width as u32;
        let ah = analysis_height as u32;
        let count = (analysis_width * analysis_height) as usize;

        let previous_analysis_tex = RenderTarget::new(
            device, aw, ah, wgpu::TextureFormat::Rgba8Unorm,
            &format!("WireframeDepthPrev_{}", owner_key));
        let depth_tex = RenderTarget::new(
            device, aw, ah, wgpu::TextureFormat::Rgba16Float,
            &format!("WireframeDepthDepth_{}", owner_key));
        let line_history_tex = RenderTarget::new(
            device, wire_width as u32, wire_height as u32,
            wgpu::TextureFormat::Rgba8Unorm,
            &format!("WireframeDepthHistory_{}", owner_key));
        let flow_tex = RenderTarget::new(
            device, aw, ah, wgpu::TextureFormat::Rgba16Float,
            &format!("WireframeDepthFlow_{}", owner_key));
        let mesh_coord_tex = RenderTarget::new(
            device, aw, ah, wgpu::TextureFormat::Rgba16Float,
            &format!("WireframeDepthMeshCoord_{}", owner_key));
        let semantic_tex = RenderTarget::new(
            device, aw, ah, wgpu::TextureFormat::Rgba16Float,
            &format!("WireframeDepthSemantic_{}", owner_key));
        let surface_cache_tex = RenderTarget::new(
            device, aw, ah, wgpu::TextureFormat::Rgba16Float,
            &format!("WireframeDepthSurface_{}", owner_key));
        let dnn_input_tex = RenderTarget::new(
            device, aw, ah, wgpu::TextureFormat::Rgba8Unorm,
            &format!("WireframeDepthDnnInput_{}", owner_key));
        let dnn_depth_texture = RenderTarget::new(
            device, aw, ah, wgpu::TextureFormat::Rgba8Unorm,
            &format!("WireframeDepthDnnDepth_{}", owner_key));
        let native_flow_texture = RenderTarget::new(
            device, aw, ah, wgpu::TextureFormat::Rgba32Float,
            &format!("WireframeDepthNativeFlow_{}", owner_key));
        let dnn_subject_texture = RenderTarget::new(
            device, aw, ah, wgpu::TextureFormat::Rgba8Unorm,
            &format!("WireframeDepthDnnSubject_{}", owner_key));

        let readback = ReadbackRequest::new();

        Self {
            analysis_width,
            analysis_height,
            wire_width,
            wire_height,
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
            dnn_pixel_buffer: vec![0u8; count * 4],
            dnn_depth_buffer: vec![0f32; count],
            dnn_depth_texture,
            dnn_has_subject_mask: false,
            dnn_subject_dirty: false,
            dnn_subject_buffer: vec![0f32; count],
            dnn_subject_history_buffer: vec![0f32; count],
            dnn_subject_texture,
            has_prev_native_frame: false,
            prev_native_pixel_buffer: vec![0u8; count * 4],
            native_flow_buffer: vec![0f32; count * 4],
            native_flow_texture,
            native_flow_has_data: false,
            native_flow_dirty: false,
            native_flow_ready: false,
            cut_score_buffer: vec![0f32; 1],
            latest_cut_score: 0.0,
            last_native_request_frame: -1024,
            last_subject_request_frame: -1024,
            last_mesh_update_frame: -1024,
            native_request_wants_flow: false,
            native_request_wants_depth: false,
            native_request_wants_subject: false,
            readback,
        }
    }
}

// Uniforms struct — must be 16-byte aligned, 80 bytes (5 × vec4).
// Matches fx_wireframe_depth.wgsl struct Uniforms exactly.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WireframeDepthUniforms {
    // vec4 at offset 0
    amount:             f32,
    grid_density:       f32,
    line_width:         f32,
    depth_scale:        f32,
    // vec4 at offset 16
    temporal_smooth:    f32,
    persistence:        f32,
    flow_lock_strength: f32,
    mesh_regularize:    f32,
    // vec4 at offset 32
    cell_affine_strength: f32,
    face_warp_strength:   f32,
    surface_persistence:  f32,
    wire_taa:             f32,
    // vec4 at offset 48
    subject_isolation:    f32,
    blend_mode:           f32,
    main_texel_x:         f32,
    main_texel_y:         f32,
    // vec4 at offset 64
    depth_texel_x:        f32,
    depth_texel_y:        f32,
    _pad0:                f32,
    _pad1:                f32,
}

// WireframeDepthFX.cs line 16: WireframeDepthFX : SimpleBlitEffect, IStatefulEffect
pub struct WireframeDepthFX {
    // 15 render pipelines, one per pass (indexed by PASS_* constants)
    pipelines: [wgpu::RenderPipeline; PASS_COUNT],
    bind_group_layout: wgpu::BindGroupLayout,
    sampler:           wgpu::Sampler,
    uniform_buffer:    wgpu::Buffer,
    // 1×1 black dummy textures for unused slots
    dummy_rgba8:        wgpu::Texture,
    dummy_rgba8_view:   wgpu::TextureView,
    dummy_rgba16f:      wgpu::Texture,
    dummy_rgba16f_view: wgpu::TextureView,
    dummy_rgba32f:      wgpu::Texture,
    dummy_rgba32f_view: wgpu::TextureView,

    // WireframeDepthFX.cs line 92-99
    owner_states: HashMap<i64, OwnerState>,
    width:  u32,
    height: u32,

    // DNN backend — optional (None = no DNN)
    // WireframeDepthFX.cs line 96-102
    depth_estimator:        Option<Box<dyn DepthEstimator>>,
    dnn_backend_initialized: bool,
    dnn_backend_available:   bool,
    dnn_next_retry_frame:    i32,
    warned_missing_dnn:      bool,
    dnn_subject_api_available: bool,
    // frame counter (replaces Unity's Time.frameCount)
    frame_count: i32,
}

// ---------------------------------------------------------------------------
// Pipeline + bind-group helpers
// ---------------------------------------------------------------------------

fn make_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            // 1..=12: textures (filterable float)
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
    })
}

fn make_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    bgl: &wgpu::BindGroupLayout,
    fs_entry: &str,
    format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[bgl],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fs_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn create_1x1_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::Texture {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    // Fill with zeros
    let bytes_per_pixel: u32 = match format {
        wgpu::TextureFormat::Rgba8Unorm   => 4,
        wgpu::TextureFormat::Rgba16Float  => 8,
        wgpu::TextureFormat::Rgba32Float  => 16,
        _ => 4,
    };
    let data = vec![0u8; bytes_per_pixel as usize];
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_pixel),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
    );
    tex
}

fn clear_rt(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
    let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("ClearRT"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}

// ---------------------------------------------------------------------------
// Blit helper — run one pass: reads main_tex, writes to target_view.
// All other texture slots get the dummy or per-pass real textures.
// ---------------------------------------------------------------------------
struct PassBind<'a> {
    main_tex:             &'a wgpu::TextureView,
    prev_analysis_tex:    &'a wgpu::TextureView,
    prev_depth_tex:       &'a wgpu::TextureView,
    depth_tex:            &'a wgpu::TextureView,
    history_tex:          &'a wgpu::TextureView,
    flow_tex:             &'a wgpu::TextureView,
    mesh_coord_tex:       &'a wgpu::TextureView,
    prev_mesh_coord_tex:  &'a wgpu::TextureView,
    semantic_tex:         &'a wgpu::TextureView,
    surface_cache_tex:    &'a wgpu::TextureView,
    prev_surface_cache_tex: &'a wgpu::TextureView,
    subject_mask_tex:     &'a wgpu::TextureView,
}

impl WireframeDepthFX {
    fn run_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        pass_idx: usize,
        bind: &PassBind<'_>,
        target_view: &wgpu::TextureView,
        uniforms: WireframeDepthUniforms,
        target_format: wgpu::TextureFormat,
    ) {
        // Update uniform buffer (write per-pass uniforms)
        // We re-use the single buffer; callers must flush encoder between passes.
        // (In wgpu the queue.write_buffer before submit is fine since we only read
        //  in the fragment shader after the render pass is encoded.)
        let uniform_data = bytemuck::bytes_of(&uniforms);
        // We write a uniform staging buffer per call so concurrent passes are safe.
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("WireframeDepth uniform staging"),
            size: std::mem::size_of::<WireframeDepthUniforms>() as u64,
            usage: wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        staging.slice(..).get_mapped_range_mut().copy_from_slice(uniform_data);
        staging.unmap();
        encoder.copy_buffer_to_buffer(
            &staging, 0,
            &self.uniform_buffer, 0,
            std::mem::size_of::<WireframeDepthUniforms>() as u64,
        );

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("WireframeDepth BG"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0,  resource: self.uniform_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1,  resource: wgpu::BindingResource::TextureView(bind.main_tex) },
                wgpu::BindGroupEntry { binding: 2,  resource: wgpu::BindingResource::TextureView(bind.prev_analysis_tex) },
                wgpu::BindGroupEntry { binding: 3,  resource: wgpu::BindingResource::TextureView(bind.prev_depth_tex) },
                wgpu::BindGroupEntry { binding: 4,  resource: wgpu::BindingResource::TextureView(bind.depth_tex) },
                wgpu::BindGroupEntry { binding: 5,  resource: wgpu::BindingResource::TextureView(bind.history_tex) },
                wgpu::BindGroupEntry { binding: 6,  resource: wgpu::BindingResource::TextureView(bind.flow_tex) },
                wgpu::BindGroupEntry { binding: 7,  resource: wgpu::BindingResource::TextureView(bind.mesh_coord_tex) },
                wgpu::BindGroupEntry { binding: 8,  resource: wgpu::BindingResource::TextureView(bind.prev_mesh_coord_tex) },
                wgpu::BindGroupEntry { binding: 9,  resource: wgpu::BindingResource::TextureView(bind.semantic_tex) },
                wgpu::BindGroupEntry { binding: 10, resource: wgpu::BindingResource::TextureView(bind.surface_cache_tex) },
                wgpu::BindGroupEntry { binding: 11, resource: wgpu::BindingResource::TextureView(bind.prev_surface_cache_tex) },
                wgpu::BindGroupEntry { binding: 12, resource: wgpu::BindingResource::TextureView(bind.subject_mask_tex) },
                wgpu::BindGroupEntry { binding: 13, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });

        // Select pipeline by output format: composite outputs to Rgba16Float (main chain),
        // all internal passes output to their own formats.
        // We ignore target_format parameter; the pipeline was built with correct format.
        let _ = target_format;

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("WireframeDepth pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipelines[pass_idx]);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Copy src view → dst view (passthrough blit using pass 0 = Analysis,
    /// but with a dedicated "copy" pipeline). Unity: Graphics.Blit(src, dst).
    /// We reuse PASS_ANALYSIS (luminance) when copying analysis-size Rgba8Unorm
    /// textures. For general copies we use copy_texture_to_texture.
    fn copy_texture(
        encoder: &mut wgpu::CommandEncoder,
        src: &wgpu::Texture,
        dst: &wgpu::Texture,
        width: u32,
        height: u32,
    ) {
        encoder.copy_texture_to_texture(
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
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
    }

    // WireframeDepthFX.cs line 139-238 — GetOrCreateOwner
    fn get_or_create_owner(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        owner_key: i64,
        wire_scale: f32,
    ) -> &mut OwnerState {
        let wire_w = ((self.width as f32 * wire_scale).round() as i32).max(64);
        let wire_h = ((self.height as f32 * wire_scale).round() as i32).max(36);

        // Check existing state — rebuild wire RT only if scale changed
        if let Some(state) = self.owner_states.get_mut(&owner_key) {
            if state.wire_width != wire_w || state.wire_height != wire_h {
                // Rebuild line_history_tex only
                state.line_history_tex = RenderTarget::new(
                    device,
                    wire_w as u32, wire_h as u32,
                    wgpu::TextureFormat::Rgba8Unorm,
                    &format!("WireframeDepthHistory_{}", owner_key),
                );
                clear_rt(encoder, &state.line_history_tex.view);
                state.wire_width  = wire_w;
                state.wire_height = wire_h;
            }
            return self.owner_states.get_mut(&owner_key).unwrap();
        }

        // New owner — compute analysis dimensions
        let scale = 1f32.min(MAX_ANALYSIS_DIM as f32 / self.width.max(self.height) as f32);
        let analysis_width  = ((self.width  as f32 * scale).round() as i32).max(64);
        let analysis_height = ((self.height as f32 * scale).round() as i32).max(36);

        let mut state = OwnerState::new(
            device,
            analysis_width, analysis_height,
            wire_w, wire_h,
            owner_key,
        );

        // WireframeDepthFX.cs line 224-235 — clear + init
        clear_rt(encoder, &state.previous_analysis_tex.view);
        clear_rt(encoder, &state.depth_tex.view);
        clear_rt(encoder, &state.line_history_tex.view);
        clear_rt(encoder, &state.flow_tex.view);
        clear_rt(encoder, &state.semantic_tex.view);
        clear_rt(encoder, &state.surface_cache_tex.view);
        clear_rt(encoder, &state.dnn_input_tex.view);

        self.owner_states.insert(owner_key, state);

        // Initialize mesh coord after insert so we can borrow the state
        let state = self.owner_states.get_mut(&owner_key).unwrap();
        // InitializeMeshCoord inline: PASS_INIT_MESH_COORD → meshCoordTex,
        // then PASS_SURFACE_CACHE_UPDATE with surfacePersistence=0.9.
        // We can't run GPU passes inside get_or_create_owner (borrow issues).
        // We flag that mesh coord needs initialization; it will run at start of apply().
        // Unity does it here — we replicate by noting it always happens fresh.
        // The mesh coord starts black/zero which causes InitializeMeshCoord in apply().
        state.last_mesh_update_frame = -1024;

        self.owner_states.get_mut(&owner_key).unwrap()
    }

    // WireframeDepthFX.cs line 240-257 — InitializeMeshCoord
    // Runs PASS_INIT_MESH_COORD → mesh_coord_tex, then PASS_SURFACE_CACHE_UPDATE.
    fn initialize_mesh_coord(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        state: &OwnerState,
        uniforms: WireframeDepthUniforms,
    ) {
        let d = &self.dummy_rgba8_view;
        let d16 = &self.dummy_rgba16f_view;

        // PASS_INIT_MESH_COORD: blit null → meshCoordTex
        self.run_pass(
            device, encoder, PASS_INIT_MESH_COORD,
            &PassBind {
                main_tex: d, prev_analysis_tex: d, prev_depth_tex: d16,
                depth_tex: d16, history_tex: d, flow_tex: d16,
                mesh_coord_tex: d16, prev_mesh_coord_tex: d16,
                semantic_tex: d16, surface_cache_tex: d16,
                prev_surface_cache_tex: d16, subject_mask_tex: d,
            },
            &state.mesh_coord_tex.view,
            uniforms,
            wgpu::TextureFormat::Rgba16Float,
        );

        // PASS_SURFACE_CACHE_UPDATE: meshCoordTex → surfaceCacheTex with persistence=0.9
        let mut u2 = uniforms;
        u2.surface_persistence = 0.9;
        self.run_pass(
            device, encoder, PASS_SURFACE_CACHE_UPDATE,
            &PassBind {
                main_tex: &state.mesh_coord_tex.view,
                prev_analysis_tex: d,
                prev_depth_tex: d16,
                depth_tex: d16,
                history_tex: d,
                flow_tex: d16,
                mesh_coord_tex: d16,
                prev_mesh_coord_tex: d16,
                semantic_tex: d16,
                surface_cache_tex: d16,
                prev_surface_cache_tex: d,  // Texture2D.blackTexture
                subject_mask_tex: d,
            },
            &state.surface_cache_tex.view,
            u2,
            wgpu::TextureFormat::Rgba16Float,
        );
    }

    // WireframeDepthFX.cs line 497-525 — EnsureDnnBackendAvailable
    fn ensure_dnn_backend_available(&mut self) -> bool {
        if self.dnn_backend_initialized && self.dnn_backend_available {
            return true;
        }
        if self.dnn_backend_initialized && !self.dnn_backend_available
            && self.frame_count < self.dnn_next_retry_frame
        {
            return false;
        }

        self.dnn_backend_available = self.depth_estimator.is_some();
        self.dnn_backend_initialized = true;
        if !self.dnn_backend_available {
            self.dnn_next_retry_frame = self.frame_count + 300;
        }
        self.dnn_backend_available
    }

    // WireframeDepthFX.cs line 715-728 — DisableDnnBackend
    fn disable_dnn_backend(&mut self) {
        self.depth_estimator = None;
        self.dnn_backend_initialized = true;
        self.dnn_backend_available = false;
        self.dnn_next_retry_frame = self.frame_count + 300;
    }

    // WireframeDepthFX.cs line 538-554 — UploadDnnDepthTexture
    fn upload_dnn_depth_texture(
        queue: &wgpu::Queue,
        state: &mut OwnerState,
    ) {
        if !state.dnn_depth_dirty { return; }
        let count = (state.analysis_width * state.analysis_height) as usize;
        let mut pixels = vec![0u8; count * 4];
        for i in 0..count {
            let v = (state.dnn_depth_buffer[i].clamp(0.0, 1.0) * 255.0) as u8;
            pixels[i * 4 + 0] = v;
            pixels[i * 4 + 1] = v;
            pixels[i * 4 + 2] = v;
            pixels[i * 4 + 3] = 255;
        }
        let aw = state.analysis_width as u32;
        let ah = state.analysis_height as u32;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &state.dnn_depth_texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(aw * 4),
                rows_per_image: Some(ah),
            },
            wgpu::Extent3d { width: aw, height: ah, depth_or_array_layers: 1 },
        );
        state.dnn_depth_dirty = false;
    }

    // WireframeDepthFX.cs line 556-572 — UploadDnnSubjectTexture
    fn upload_dnn_subject_texture(
        queue: &wgpu::Queue,
        state: &mut OwnerState,
    ) {
        if !state.dnn_subject_dirty { return; }
        let count = (state.analysis_width * state.analysis_height) as usize;
        let mut pixels = vec![0u8; count * 4];
        for i in 0..count {
            let v = (state.dnn_subject_history_buffer[i].clamp(0.0, 1.0) * 255.0) as u8;
            pixels[i * 4 + 0] = v;
            pixels[i * 4 + 1] = v;
            pixels[i * 4 + 2] = v;
            pixels[i * 4 + 3] = 255;
        }
        let aw = state.analysis_width as u32;
        let ah = state.analysis_height as u32;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &state.dnn_subject_texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(aw * 4),
                rows_per_image: Some(ah),
            },
            wgpu::Extent3d { width: aw, height: ah, depth_or_array_layers: 1 },
        );
        state.dnn_subject_dirty = false;
    }

    // WireframeDepthFX.cs line 574-594 — UploadNativeFlowTexture
    fn upload_native_flow_texture(
        queue: &wgpu::Queue,
        state: &mut OwnerState,
    ) {
        if !state.native_flow_dirty { return; }
        let count = (state.analysis_width * state.analysis_height) as usize;
        // Rgba32Float: 16 bytes per pixel, raw f32 values
        let pixel_bytes: &[u8] = bytemuck::cast_slice(&state.native_flow_buffer[..count * 4]);
        let aw = state.analysis_width as u32;
        let ah = state.analysis_height as u32;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &state.native_flow_texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixel_bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(aw * 16), // Rgba32Float = 16 bytes/pixel
                rows_per_image: Some(ah),
            },
            wgpu::Extent3d { width: aw, height: ah, depth_or_array_layers: 1 },
        );
        state.native_flow_dirty = false;
    }

    // WireframeDepthFX.cs line 455-495 — RequestNativeReadback
    // In Rust: blit source → dnn_input_tex (analysis size via PASS_ANALYSIS),
    // then submit readback of dnn_input_tex.
    // Unity: Graphics.Blit(source, state.dnnInputTex) then AsyncGPUReadback.Request.
    fn request_native_readback(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source_view: &wgpu::TextureView,
        source_width: u32,
        source_height: u32,
        owner_key: i64,
        mode: DepthSourceMode,
        subject_isolation: f32,
    ) {
        let state = match self.owner_states.get_mut(&owner_key) {
            Some(s) => s,
            None => return,
        };

        let wants_depth = mode == DepthSourceMode::Dnn;
        let wants_flow  = true;
        let wants_subject = self.dnn_subject_api_available
            && mode == DepthSourceMode::Dnn
            && subject_isolation > 0.02
            && self.frame_count - state.last_subject_request_frame >= NATIVE_UPDATE_INTERVAL_SUBJECT;

        if !wants_depth && !wants_flow && !wants_subject { return; }

        let min_interval = if mode == DepthSourceMode::Dnn {
            NATIVE_UPDATE_INTERVAL_DNN
        } else {
            NATIVE_UPDATE_INTERVAL_HEURISTIC
        };
        if self.frame_count - state.last_native_request_frame < min_interval { return; }

        if !self.dnn_backend_available { return; }
        if state.dnn_readback_pending { return; }

        state.native_request_wants_depth   = wants_depth;
        state.native_request_wants_flow    = wants_flow;
        state.native_request_wants_subject = wants_subject;
        state.last_native_request_frame    = self.frame_count;
        if wants_subject {
            state.last_subject_request_frame = self.frame_count;
        }

        // Graphics.Blit(source, state.dnnInputTex) — blit full source into analysis RT.
        // Split: first get the view (immutable borrow ends), then blit, then get mut for readback.
        let dnn_input_w = state.analysis_width as u32;
        let dnn_input_h = state.analysis_height as u32;
        drop(state); // end immutable borrow of state

        // Create a temporary RT to blit source into (analysis-sized Rgba8Unorm).
        // We then copy that into dnn_input_tex via copy_texture.
        // Unity does: Graphics.Blit(source, state.dnnInputTex) — a full-screen rescale.
        // Here we blit source → a temp analysis RT via PASS_ANALYSIS, then copy to dnn_input_tex.
        let blit_tmp = RenderTarget::new(
            device, dnn_input_w, dnn_input_h,
            wgpu::TextureFormat::Rgba8Unorm, "WireframeDepth_dnn_blit_tmp",
        );
        let u_blit = WireframeDepthUniforms {
            amount: 0.0, grid_density: 0.0, line_width: 0.0, depth_scale: 0.0,
            temporal_smooth: 0.0, persistence: 0.0, flow_lock_strength: 0.0,
            mesh_regularize: 0.0, cell_affine_strength: 0.0, face_warp_strength: 0.0,
            surface_persistence: 0.0, wire_taa: 0.0, subject_isolation: 0.0,
            blend_mode: 0.0,
            main_texel_x: 1.0 / source_width as f32,
            main_texel_y: 1.0 / source_height as f32,
            depth_texel_x: 0.0, depth_texel_y: 0.0, _pad0: 0.0, _pad1: 0.0,
        };
        let d   = &self.dummy_rgba8_view;
        let d16 = &self.dummy_rgba16f_view;
        self.run_pass(
            device, encoder, PASS_ANALYSIS,
            &PassBind {
                main_tex: source_view,
                prev_analysis_tex: d, prev_depth_tex: d16, depth_tex: d16,
                history_tex: d, flow_tex: d16, mesh_coord_tex: d16,
                prev_mesh_coord_tex: d16, semantic_tex: d16,
                surface_cache_tex: d16, prev_surface_cache_tex: d16, subject_mask_tex: d,
            },
            &blit_tmp.view,
            u_blit,
            wgpu::TextureFormat::Rgba8Unorm,
        );

        // Copy blit_tmp → state.dnn_input_tex, then submit readback
        let state = self.owner_states.get_mut(&owner_key).unwrap();
        Self::copy_texture(
            encoder, &blit_tmp.texture, &state.dnn_input_tex.texture,
            dnn_input_w, dnn_input_h,
        );
        state.readback.submit(device, encoder, &state.dnn_input_tex.texture, dnn_input_w, dnn_input_h);
        state.dnn_readback_pending = true;
    }

    // WireframeDepthFX.cs line 596-713 — OnNativeReadbackComplete
    // Poll the pending readback; if data is ready, run DNN inference.
    fn poll_native_readback(&mut self, device: &wgpu::Device, owner_key: i64) {
        let state = match self.owner_states.get_mut(&owner_key) {
            Some(s) => s,
            None => return,
        };
        if !state.dnn_readback_pending { return; }

        let pixel_data = match state.readback.try_read(device) {
            Some(d) => d,
            None    => return, // not ready yet
        };

        state.dnn_readback_pending = false;

        if !self.dnn_backend_available { return; }

        // Copy pixel data into dnn_pixel_buffer
        let copy_len = pixel_data.len().min(state.dnn_pixel_buffer.len());
        state.dnn_pixel_buffer[..copy_len].copy_from_slice(&pixel_data[..copy_len]);

        let aw = state.analysis_width;
        let ah = state.analysis_height;

        let wants_flow    = state.native_request_wants_flow;
        let wants_depth   = state.native_request_wants_depth;
        let wants_subject = state.native_request_wants_subject;
        let has_prev      = state.has_prev_native_frame;

        if wants_flow && has_prev {
            if let Some(ref mut estimator) = self.depth_estimator {
                // Temporarily take buffers out to satisfy borrow checker
                let mut flow_buf = std::mem::take(&mut state.native_flow_buffer);
                let mut cut_buf  = std::mem::take(&mut state.cut_score_buffer);
                let ok = estimator.compute_flow(
                    &state.prev_native_pixel_buffer,
                    &state.dnn_pixel_buffer,
                    aw, ah,
                    &mut flow_buf,
                    aw, ah,
                    &mut cut_buf,
                );
                state.native_flow_buffer = flow_buf;
                state.cut_score_buffer   = cut_buf;
                if ok != 0 {
                    state.native_flow_has_data = true;
                    state.native_flow_dirty    = true;
                    state.native_flow_ready    = true;
                    state.latest_cut_score     = state.cut_score_buffer[0];
                } else {
                    state.native_flow_has_data = false;
                    state.native_flow_ready    = false;
                    state.latest_cut_score     = 0.0;
                }
            }
        } else {
            state.native_flow_has_data = false;
            state.native_flow_ready    = false;
            state.latest_cut_score     = 0.0;
        }

        if wants_depth {
            if let Some(ref mut estimator) = self.depth_estimator {
                let mut depth_buf = std::mem::take(&mut state.dnn_depth_buffer);
                let ok = estimator.process(
                    &state.dnn_pixel_buffer,
                    aw, ah,
                    &mut depth_buf,
                    aw, ah,
                );
                state.dnn_depth_buffer = depth_buf;
                if ok != 0 {
                    state.dnn_has_depth   = true;
                    state.dnn_depth_dirty = true;
                }
            }
        }

        if wants_subject && self.dnn_subject_api_available {
            if let Some(ref mut estimator) = self.depth_estimator {
                let mut subject_buf = std::mem::take(&mut state.dnn_subject_buffer);
                let ok = estimator.process_subject_mask(
                    &state.dnn_pixel_buffer,
                    aw, ah,
                    &mut subject_buf,
                    aw, ah,
                );
                state.dnn_subject_buffer = subject_buf;
                if ok != 0 {
                    let count = (aw * ah) as usize;
                    const BLEND: f32 = 0.55;
                    let has_prev_mask = state.dnn_has_subject_mask;
                    for i in 0..count {
                        let current  = state.dnn_subject_buffer[i].clamp(0.0, 1.0);
                        let previous = if has_prev_mask {
                            state.dnn_subject_history_buffer[i]
                        } else {
                            current
                        };
                        // Mathf.Lerp(previous, current, blend) — blend clamps t
                        state.dnn_subject_history_buffer[i] =
                            previous + (current - previous) * BLEND.clamp(0.0, 1.0);
                    }
                    state.dnn_has_subject_mask = true;
                    state.dnn_subject_dirty    = true;
                }
                // Note: if process_subject_mask fails, dnn_subject_api_available stays true.
                // Unity catches EntryPointNotFoundException — in Rust that's a link error at
                // compile time, not runtime. We leave dnn_subject_api_available true.
            }
        }

        // Buffer.BlockCopy(dnnPixelBuffer → prevNativePixelBuffer)
        let copy_len2 = state.dnn_pixel_buffer.len().min(state.prev_native_pixel_buffer.len());
        let src: Vec<u8> = state.dnn_pixel_buffer[..copy_len2].to_vec();
        state.prev_native_pixel_buffer[..copy_len2].copy_from_slice(&src);
        state.has_prev_native_frame = true;
    }

    // WireframeDepthFX.cs line 894-913 — EstimateDepthHeuristic
    // Kept as documentation of the Unity structure; inlined at call site to avoid split-borrow.
    #[allow(dead_code)]
    fn estimate_depth_heuristic(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        analysis_view: &wgpu::TextureView,
        state: &OwnerState,
        uniforms: WireframeDepthUniforms,
    ) {
        let d = &self.dummy_rgba8_view;
        let d16 = &self.dummy_rgba16f_view;

        // Temp depthNext RT (ARGBHalf → Rgba16Float, analysis size)
        let depth_next = RenderTarget::new(
            device,
            state.analysis_width as u32,
            state.analysis_height as u32,
            wgpu::TextureFormat::Rgba16Float,
            "WireframeDepth_depthNext_heuristic",
        );

        // Graphics.Blit(analysis, depthNext, material, PASS_HEURISTIC_DEPTH)
        self.run_pass(
            device, encoder, PASS_HEURISTIC_DEPTH,
            &PassBind {
                main_tex: analysis_view,
                prev_analysis_tex: &state.previous_analysis_tex.view,
                prev_depth_tex: &state.depth_tex.view,
                depth_tex: d16,
                history_tex: d,
                flow_tex: d16,
                mesh_coord_tex: d16,
                prev_mesh_coord_tex: d16,
                semantic_tex: d16,
                surface_cache_tex: d16,
                prev_surface_cache_tex: d16,
                subject_mask_tex: d,
            },
            &depth_next.view,
            uniforms,
            wgpu::TextureFormat::Rgba16Float,
        );

        // Graphics.Blit(depthNext, state.depthTex)
        Self::copy_texture(
            encoder,
            &depth_next.texture,
            &state.depth_tex.texture,
            state.analysis_width as u32,
            state.analysis_height as u32,
        );
    }

    // WireframeDepthFX.cs line 424-453 — TryEstimateDepthDnn
    fn try_estimate_depth_dnn(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        owner_key: i64,
        uniforms: WireframeDepthUniforms,
    ) -> bool {
        if !self.dnn_backend_available { return false; }

        // Upload DNN depth if dirty
        if self.owner_states.get(&owner_key).map(|s| s.dnn_depth_dirty).unwrap_or(false) {
            let state = self.owner_states.get_mut(&owner_key).unwrap();
            Self::upload_dnn_depth_texture(queue, state);
        }

        let (has_depth, aw, ah, dnn_view_ptr, prev_depth_ptr, depth_tex_ptr) = {
            let st = self.owner_states.get(&owner_key).unwrap();
            (
                st.dnn_has_depth,
                st.analysis_width as u32,
                st.analysis_height as u32,
                &st.dnn_depth_texture.view as *const wgpu::TextureView,
                &st.depth_tex.view as *const wgpu::TextureView,
                &st.depth_tex.texture as *const wgpu::Texture,
            )
        };
        if !has_depth { return false; }

        // SAFETY: OwnerState is heap-stable in HashMap; run_pass does not mutate owner_states.
        let dnn_view    = unsafe { &*dnn_view_ptr };
        let prev_d_view = unsafe { &*prev_depth_ptr };
        let depth_tex   = unsafe { &*depth_tex_ptr };

        let d   = &self.dummy_rgba8_view;
        let d16 = &self.dummy_rgba16f_view;

        let depth_next = RenderTarget::new(device, aw, ah,
            wgpu::TextureFormat::Rgba16Float, "WireframeDepth_depthNext_dnn");

        // Graphics.Blit(dnnDepthTexture, depthNext, material, PASS_DNN_DEPTH_POST)
        self.run_pass(
            device, encoder, PASS_DNN_DEPTH_POST,
            &PassBind {
                main_tex: dnn_view,
                prev_analysis_tex: d,
                prev_depth_tex: prev_d_view,
                depth_tex: d16, history_tex: d, flow_tex: d16,
                mesh_coord_tex: d16, prev_mesh_coord_tex: d16, semantic_tex: d16,
                surface_cache_tex: d16, prev_surface_cache_tex: d16, subject_mask_tex: d,
            },
            &depth_next.view, uniforms, wgpu::TextureFormat::Rgba16Float,
        );

        // Graphics.Blit(depthNext, state.depthTex)
        Self::copy_texture(encoder, &depth_next.texture, depth_tex, aw, ah);
        true
    }

    // WireframeDepthFX.cs line 730-892 — UpdateFlowLock
    #[allow(clippy::too_many_arguments)]
    fn update_flow_lock_inner(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        analysis_view: &wgpu::TextureView,
        analysis_texture: &wgpu::Texture, // for copy_texture calls
        state: &mut OwnerState,
        uniforms: WireframeDepthUniforms,
        temporal_smooth: f32,
        mesh_rate: i32,
        native_flow_enabled: bool,
        face_warp_enabled: bool,
    ) {
        if native_flow_enabled && state.native_flow_dirty {
            Self::upload_native_flow_texture(queue, state);
        }

        let use_native_flow = native_flow_enabled && state.native_flow_has_data;

        // Scene cut: hard reset
        if use_native_flow && state.latest_cut_score > 0.28 {
            // InitializeMeshCoord
            self.initialize_mesh_coord(device, encoder, state, uniforms);
            clear_rt(encoder, &state.line_history_tex.view);
            clear_rt(encoder, &state.semantic_tex.view);
            state.dnn_has_subject_mask = false;
            for v in state.dnn_subject_history_buffer.iter_mut() { *v = 0.0; }
            state.latest_cut_score = 0.0;
            state.native_flow_ready    = false;
            state.native_flow_has_data = false;
            state.last_mesh_update_frame = self.frame_count;
            // Still update previousAnalysisTex (Graphics.Blit(analysis, previousAnalysisTex))
            Self::copy_texture(
                encoder,
                analysis_texture,
                &state.previous_analysis_tex.texture,
                state.analysis_width as u32,
                state.analysis_height as u32,
            );
            return;
        }

        // Amortization: skip mesh pipeline on non-update frames
        let run_mesh_pipeline = mesh_rate <= 1
            || self.frame_count - state.last_mesh_update_frame >= mesh_rate;
        if !run_mesh_pipeline {
            // Graphics.Blit(analysis, state.previousAnalysisTex) — handled by caller
            return;
        }

        // The actual analysis→previousAnalysisTex copy is done by the caller after this returns.
        // record that we ran the mesh update
        // state.last_mesh_update_frame = self.frame_count; — set by caller

        let d = &self.dummy_rgba8_view;
        let d16 = &self.dummy_rgba16f_view;

        // Select flow input: native or shader-computed
        let flow_input_view: &wgpu::TextureView = if use_native_flow {
            &state.native_flow_texture.view
        } else {
            // PASS_FLOW_ESTIMATE: analysis → flowTex
            self.run_pass(
                device, encoder, PASS_FLOW_ESTIMATE,
                &PassBind {
                    main_tex: analysis_view,
                    prev_analysis_tex: &state.previous_analysis_tex.view,
                    prev_depth_tex: d16,
                    depth_tex: d16,
                    history_tex: d,
                    flow_tex: d16,
                    mesh_coord_tex: d16,
                    prev_mesh_coord_tex: d16,
                    semantic_tex: d16,
                    surface_cache_tex: d16,
                    prev_surface_cache_tex: d16,
                    subject_mask_tex: d,
                },
                &state.flow_tex.view,
                uniforms,
                wgpu::TextureFormat::Rgba16Float,
            );
            &state.flow_tex.view
        };

        // PASS_FLOW_HYGIENE: flowInput → flowFiltered (temp Rgba16Float)
        let flow_filtered = RenderTarget::new(
            device,
            state.analysis_width as u32,
            state.analysis_height as u32,
            wgpu::TextureFormat::Rgba16Float,
            "WireframeDepth_flowFiltered",
        );
        let mut u_hygiene = uniforms;
        u_hygiene.main_texel_x = 1.0 / state.analysis_width as f32;
        u_hygiene.main_texel_y = 1.0 / state.analysis_height as f32;
        self.run_pass(
            device, encoder, PASS_FLOW_HYGIENE,
            &PassBind {
                main_tex: flow_input_view,
                prev_analysis_tex: d,
                prev_depth_tex: d16,
                depth_tex: d16,
                history_tex: d,
                flow_tex: d16,
                mesh_coord_tex: d16,
                prev_mesh_coord_tex: d16,
                semantic_tex: d16,
                surface_cache_tex: d16,
                prev_surface_cache_tex: d16,
                subject_mask_tex: d,
            },
            &flow_filtered.view,
            u_hygiene,
            wgpu::TextureFormat::Rgba16Float,
        );
        let flow_stable = &flow_filtered.view;

        // Temp RTs for mesh pipeline
        let coord_next = RenderTarget::new(
            device, state.analysis_width as u32, state.analysis_height as u32,
            wgpu::TextureFormat::Rgba16Float, "WireframeDepth_coordNext");
        let coord_affine = RenderTarget::new(
            device, state.analysis_width as u32, state.analysis_height as u32,
            wgpu::TextureFormat::Rgba16Float, "WireframeDepth_coordAffine");
        let coord_regularized = RenderTarget::new(
            device, state.analysis_width as u32, state.analysis_height as u32,
            wgpu::TextureFormat::Rgba16Float, "WireframeDepth_coordReg");
        let surface_next = RenderTarget::new(
            device, state.analysis_width as u32, state.analysis_height as u32,
            wgpu::TextureFormat::Rgba16Float, "WireframeDepth_surfaceNext");

        let analysis_texel_x = 1.0 / state.analysis_width as f32;
        let analysis_texel_y = 1.0 / state.analysis_height as f32;

        // PASS_SEMANTIC_MASK: analysis → semanticTex
        let mut u_sem = uniforms;
        u_sem.main_texel_x = analysis_texel_x;
        u_sem.main_texel_y = analysis_texel_y;
        self.run_pass(
            device, encoder, PASS_SEMANTIC_MASK,
            &PassBind {
                main_tex: analysis_view,
                prev_analysis_tex: &state.previous_analysis_tex.view,
                prev_depth_tex: d16,
                depth_tex: &state.depth_tex.view,
                history_tex: d,
                flow_tex: flow_stable,
                mesh_coord_tex: d16,
                prev_mesh_coord_tex: d16,
                semantic_tex: d16,
                surface_cache_tex: d16,
                prev_surface_cache_tex: d16,
                subject_mask_tex: d,
            },
            &state.semantic_tex.view,
            u_sem,
            wgpu::TextureFormat::Rgba16Float,
        );

        // PASS_FLOW_ADVECT_COORD: analysis → coordNext
        // flowLockStrength = lerp(0.76, 0.985, clamp01(temporalSmooth))
        let flow_lock_strength = 0.76f32 + (0.985 - 0.76) * temporal_smooth.clamp(0.0, 1.0);
        let mut u_advect = uniforms;
        u_advect.main_texel_x    = analysis_texel_x;
        u_advect.main_texel_y    = analysis_texel_y;
        u_advect.flow_lock_strength = flow_lock_strength;
        self.run_pass(
            device, encoder, PASS_FLOW_ADVECT_COORD,
            &PassBind {
                main_tex: analysis_view,
                prev_analysis_tex: &state.previous_analysis_tex.view,
                prev_depth_tex: d16,
                depth_tex: d16,
                history_tex: d,
                flow_tex: flow_stable,
                mesh_coord_tex: d16,
                prev_mesh_coord_tex: &state.mesh_coord_tex.view,
                semantic_tex: &state.semantic_tex.view,
                surface_cache_tex: d16,
                prev_surface_cache_tex: d16,
                subject_mask_tex: d,
            },
            &coord_next.view,
            u_advect,
            wgpu::TextureFormat::Rgba16Float,
        );

        // PASS_MESH_CELL_AFFINE: coordNext → coordAffine
        // cellAffine = lerp(0.40, 0.88, clamp01(temporalSmooth))
        let cell_affine = 0.40f32 + (0.88 - 0.40) * temporal_smooth.clamp(0.0, 1.0);
        let mut u_affine = uniforms;
        u_affine.main_texel_x       = analysis_texel_x;
        u_affine.main_texel_y       = analysis_texel_y;
        u_affine.cell_affine_strength = cell_affine;
        self.run_pass(
            device, encoder, PASS_MESH_CELL_AFFINE,
            &PassBind {
                main_tex: &coord_next.view,
                prev_analysis_tex: d,
                prev_depth_tex: d16,
                depth_tex: d16,
                history_tex: d,
                flow_tex: flow_stable,
                mesh_coord_tex: d16,
                prev_mesh_coord_tex: d16,
                semantic_tex: d16,
                surface_cache_tex: d16,
                prev_surface_cache_tex: d16,
                subject_mask_tex: d,
            },
            &coord_affine.view,
            u_affine,
            wgpu::TextureFormat::Rgba16Float,
        );

        // Face warp (optional pass)
        // preRegularize = coordFace (if enabled) else coordAffine
        let pre_regularize_view: &wgpu::TextureView;
        let coord_face_opt: Option<RenderTarget>;
        if face_warp_enabled {
            // faceWarpStrength = lerp(0.25, 0.90, clamp01(temporalSmooth))
            let face_warp_strength = 0.25f32 + (0.90 - 0.25) * temporal_smooth.clamp(0.0, 1.0);
            let coord_face = RenderTarget::new(
                device, state.analysis_width as u32, state.analysis_height as u32,
                wgpu::TextureFormat::Rgba16Float, "WireframeDepth_coordFace");
            let mut u_face = uniforms;
            u_face.main_texel_x    = analysis_texel_x;
            u_face.main_texel_y    = analysis_texel_y;
            u_face.face_warp_strength = face_warp_strength;
            self.run_pass(
                device, encoder, PASS_MESH_FACE_WARP,
                &PassBind {
                    main_tex: &coord_affine.view,
                    prev_analysis_tex: d,
                    prev_depth_tex: d16,
                    depth_tex: d16,
                    history_tex: d,
                    flow_tex: flow_stable,
                    mesh_coord_tex: d16,
                    prev_mesh_coord_tex: d16,
                    semantic_tex: &state.semantic_tex.view,
                    surface_cache_tex: d16,
                    prev_surface_cache_tex: d16,
                    subject_mask_tex: d,
                },
                &coord_face.view,
                u_face,
                wgpu::TextureFormat::Rgba16Float,
            );
            coord_face_opt = Some(coord_face);
            pre_regularize_view = &coord_face_opt.as_ref().unwrap().view;
        } else {
            coord_face_opt = None;
            pre_regularize_view = &coord_affine.view;
        }

        // PASS_MESH_REGULARIZE: preRegularize → coordRegularized
        // regularize = lerp(0.40, 0.74, clamp01(temporalSmooth))
        let regularize = 0.40f32 + (0.74 - 0.40) * temporal_smooth.clamp(0.0, 1.0);
        let mut u_reg = uniforms;
        u_reg.main_texel_x   = analysis_texel_x;
        u_reg.main_texel_y   = analysis_texel_y;
        u_reg.mesh_regularize = regularize;
        self.run_pass(
            device, encoder, PASS_MESH_REGULARIZE,
            &PassBind {
                main_tex: pre_regularize_view,
                prev_analysis_tex: d,
                prev_depth_tex: d16,
                depth_tex: d16,
                history_tex: d,
                flow_tex: flow_stable,
                mesh_coord_tex: d16,
                prev_mesh_coord_tex: &state.mesh_coord_tex.view,
                semantic_tex: d16,
                surface_cache_tex: d16,
                prev_surface_cache_tex: d16,
                subject_mask_tex: d,
            },
            &coord_regularized.view,
            u_reg,
            wgpu::TextureFormat::Rgba16Float,
        );

        // Graphics.Blit(coordRegularized, state.meshCoordTex)
        Self::copy_texture(
            encoder,
            &coord_regularized.texture,
            &state.mesh_coord_tex.texture,
            state.analysis_width as u32,
            state.analysis_height as u32,
        );

        // PASS_SURFACE_CACHE_UPDATE: meshCoordTex → surfaceNext → surfaceCacheTex
        // surfacePersist = lerp(0.80, 0.985, clamp01(temporalSmooth))
        let surface_persist = 0.80f32 + (0.985 - 0.80) * temporal_smooth.clamp(0.0, 1.0);
        let mut u_surf = uniforms;
        u_surf.surface_persistence = surface_persist;
        self.run_pass(
            device, encoder, PASS_SURFACE_CACHE_UPDATE,
            &PassBind {
                main_tex: &state.mesh_coord_tex.view,
                prev_analysis_tex: d,
                prev_depth_tex: d16,
                depth_tex: d16,
                history_tex: d,
                flow_tex: flow_stable,
                mesh_coord_tex: d16,
                prev_mesh_coord_tex: d16,
                semantic_tex: d16,
                surface_cache_tex: d16,
                prev_surface_cache_tex: &state.surface_cache_tex.view,
                subject_mask_tex: d,
            },
            &surface_next.view,
            u_surf,
            wgpu::TextureFormat::Rgba16Float,
        );
        Self::copy_texture(
            encoder,
            &surface_next.texture,
            &state.surface_cache_tex.texture,
            state.analysis_width as u32,
            state.analysis_height as u32,
        );
    }

    // WireframeDepthFX.cs line 363-418 — EstimateDepth
    #[allow(clippy::too_many_arguments)]
    fn estimate_depth(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source_view: &wgpu::TextureView,
        source_width: u32,
        source_height: u32,
        owner_key: i64,
        temporal_smooth: f32,
        mode: DepthSourceMode,
        subject_isolation: f32,
        mesh_rate: i32,
        native_flow_enabled: bool,
        flow_lock_enabled: bool,
        face_warp_enabled: bool,
        base_uniforms: WireframeDepthUniforms,
    ) {
        let state = self.owner_states.get(&owner_key).unwrap();
        let analysis_width  = state.analysis_width as u32;
        let analysis_height = state.analysis_height as u32;

        // Temp analysis RT (ARGB32 → Rgba8Unorm, analysis size)
        let analysis = RenderTarget::new(
            device, analysis_width, analysis_height,
            wgpu::TextureFormat::Rgba8Unorm,
            "WireframeDepth_analysis",
        );

        // PASS_ANALYSIS: source → analysis (luminance downsample)
        let mut u_analysis = base_uniforms;
        u_analysis.main_texel_x = 1.0 / source_width as f32;
        u_analysis.main_texel_y = 1.0 / source_height as f32;
        let d = &self.dummy_rgba8_view;
        let d16 = &self.dummy_rgba16f_view;
        self.run_pass(
            device, encoder, PASS_ANALYSIS,
            &PassBind {
                main_tex: source_view,
                prev_analysis_tex: d, prev_depth_tex: d16, depth_tex: d16,
                history_tex: d, flow_tex: d16, mesh_coord_tex: d16,
                prev_mesh_coord_tex: d16, semantic_tex: d16,
                surface_cache_tex: d16, prev_surface_cache_tex: d16, subject_mask_tex: d,
            },
            &analysis.view,
            u_analysis,
            wgpu::TextureFormat::Rgba8Unorm,
        );

        // Native readback request (only when flow lock + native flow enabled + mesh update due)
        if native_flow_enabled && flow_lock_enabled {
            let mesh_update_due = {
                let st = self.owner_states.get(&owner_key).unwrap();
                mesh_rate <= 1 || self.frame_count - st.last_mesh_update_frame >= mesh_rate
            };
            if mesh_update_due {
                self.ensure_dnn_backend_available();
                self.request_native_readback(
                    device, encoder,
                    source_view, source_width, source_height,
                    owner_key, mode, subject_isolation,
                );
            }
        }

        // Poll readback from previous frame
        self.poll_native_readback(device, owner_key);

        // DNN depth or heuristic fallback
        let state = self.owner_states.get_mut(&owner_key).unwrap();
        let mut u_depth = base_uniforms;
        u_depth.main_texel_x = 1.0 / analysis_width as f32;
        u_depth.main_texel_y = 1.0 / analysis_height as f32;
        u_depth.depth_texel_x = 1.0 / analysis_width as f32;
        u_depth.depth_texel_y = 1.0 / analysis_height as f32;
        u_depth.temporal_smooth = temporal_smooth;

        let dnn_used = if mode == DepthSourceMode::Dnn {
            drop(state);
            self.try_estimate_depth_dnn(device, queue, encoder, owner_key, u_depth)
        } else {
            false
        };

        if !dnn_used {
            if mode == DepthSourceMode::Dnn && !self.warned_missing_dnn && !self.dnn_backend_available {
                log::warn!("[WireframeDepthFX] DNN depth path requested, but no backend is configured. Falling back to heuristic depth.");
                self.warned_missing_dnn = true;
            }
            // Use raw pointers to break the borrow-checker split between
            // self.owner_states (shared borrow for state views) and self.run_pass
            // (needs &self for pipelines/sampler). The OwnerState lives in a HashMap
            // heap allocation and is never reallocated while these raw refs are live.
            let (prev_a_ptr, prev_d_ptr, depth_tex_ptr, aw, ah) = {
                let st = self.owner_states.get(&owner_key).unwrap();
                (
                    &st.previous_analysis_tex.view as *const wgpu::TextureView,
                    &st.depth_tex.view as *const wgpu::TextureView,
                    &st.depth_tex.texture as *const wgpu::Texture,
                    st.analysis_width as u32,
                    st.analysis_height as u32,
                )
            };
            // SAFETY: OwnerState heap allocation is stable; run_pass does not mutate owner_states.
            let prev_a = unsafe { &*prev_a_ptr };
            let prev_d = unsafe { &*prev_d_ptr };
            let depth_tex = unsafe { &*depth_tex_ptr };

            let d   = &self.dummy_rgba8_view;
            let d16 = &self.dummy_rgba16f_view;
            let depth_next = RenderTarget::new(device, aw, ah,
                wgpu::TextureFormat::Rgba16Float, "WireframeDepth_depthNext_heuristic");
            self.run_pass(
                device, encoder, PASS_HEURISTIC_DEPTH,
                &PassBind {
                    main_tex: &analysis.view,
                    prev_analysis_tex: prev_a,
                    prev_depth_tex: prev_d,
                    depth_tex: d16, history_tex: d, flow_tex: d16,
                    mesh_coord_tex: d16, prev_mesh_coord_tex: d16, semantic_tex: d16,
                    surface_cache_tex: d16, prev_surface_cache_tex: d16, subject_mask_tex: d,
                },
                &depth_next.view, u_depth, wgpu::TextureFormat::Rgba16Float,
            );
            Self::copy_texture(encoder, &depth_next.texture, depth_tex, aw, ah);
        }

        if flow_lock_enabled {
            let (run_mesh_pipeline, is_scene_cut, analysis_w, analysis_h) = {
                let state = self.owner_states.get_mut(&owner_key).unwrap();
                let rmp = mesh_rate <= 1 || self.frame_count - state.last_mesh_update_frame >= mesh_rate;
                let un = native_flow_enabled && state.native_flow_has_data;
                let isc = un && state.latest_cut_score > 0.28;
                (rmp, isc, state.analysis_width as u32, state.analysis_height as u32)
            };
            // SAFETY: update_flow_lock_inner reads self.pipelines/sampler/frame_count/dummy views —
            // none of which overlap with owner_states. The raw pointer below points into the HashMap
            // value which is heap-stable. The mutable borrow of owner_states ends before any other
            // mutable access to self.
            let st_ptr: *mut OwnerState = self.owner_states.get_mut(&owner_key).unwrap() as *mut OwnerState;
            {
                let st = unsafe { &mut *st_ptr };
                self.update_flow_lock_inner(
                    device, queue, encoder,
                    &analysis.view,
                    &analysis.texture,
                    st,
                    u_depth,
                    temporal_smooth,
                    mesh_rate,
                    native_flow_enabled,
                    face_warp_enabled,
                );
            }

            let state = self.owner_states.get_mut(&owner_key).unwrap();
            if run_mesh_pipeline && !is_scene_cut {
                state.last_mesh_update_frame = self.frame_count;
            }

            // Update previousAnalysisTex (Graphics.Blit(analysis, previousAnalysisTex))
            Self::copy_texture(
                encoder,
                &analysis.texture,
                &state.previous_analysis_tex.texture,
                analysis_w,
                analysis_h,
            );
        } else {
            // Graphics.Blit(analysis, state.previousAnalysisTex)
            let state = self.owner_states.get(&owner_key).unwrap();
            let analysis_w = state.analysis_width as u32;
            let analysis_h = state.analysis_height as u32;
            Self::copy_texture(
                encoder,
                &analysis.texture,
                &state.previous_analysis_tex.texture,
                analysis_w,
                analysis_h,
            );
        }
    }

    // WireframeDepthFX.cs line 927-979 — ClearOwnerState
    fn clear_owner_state(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        state: &mut OwnerState,
    ) {
        clear_rt(encoder, &state.previous_analysis_tex.view);
        clear_rt(encoder, &state.depth_tex.view);
        clear_rt(encoder, &state.line_history_tex.view);
        clear_rt(encoder, &state.flow_tex.view);
        clear_rt(encoder, &state.mesh_coord_tex.view);
        clear_rt(encoder, &state.semantic_tex.view);
        clear_rt(encoder, &state.surface_cache_tex.view);
        state.dnn_readback_pending    = false;
        state.dnn_has_depth           = false;
        state.dnn_depth_dirty         = false;
        state.dnn_has_subject_mask    = false;
        state.dnn_subject_dirty       = false;
        state.has_prev_native_frame   = false;
        state.native_flow_has_data    = false;
        state.native_flow_dirty       = false;
        state.native_flow_ready       = false;
        state.native_request_wants_flow    = false;
        state.native_request_wants_depth   = false;
        state.native_request_wants_subject = false;
        state.latest_cut_score        = 0.0;
        state.last_subject_request_frame = -1024;
        state.last_mesh_update_frame     = -1024;

        // Clear dnn_depth_texture → black opaque pixels (0,0,0,255)
        let count = (state.analysis_width * state.analysis_height) as usize;
        let mut pixels = vec![0u8; count * 4];
        for i in 0..count { pixels[i * 4 + 3] = 255; }
        let aw = state.analysis_width as u32;
        let ah = state.analysis_height as u32;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &state.dnn_depth_texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0, bytes_per_row: Some(aw * 4), rows_per_image: Some(ah),
            },
            wgpu::Extent3d { width: aw, height: ah, depth_or_array_layers: 1 },
        );

        // Clear native_flow_texture → zeros (Rgba32Float)
        let flow_bytes = vec![0u8; count * 16]; // Rgba32Float = 16 bytes/pixel
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &state.native_flow_texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &flow_bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0, bytes_per_row: Some(aw * 16), rows_per_image: Some(ah),
            },
            wgpu::Extent3d { width: aw, height: ah, depth_or_array_layers: 1 },
        );

        // Clear dnn_subject_texture + history buffer
        let subj_pixels = vec![0u8; count * 4];
        for v in state.dnn_subject_history_buffer.iter_mut() { *v = 0.0; }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &state.dnn_subject_texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &subj_pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0, bytes_per_row: Some(aw * 4), rows_per_image: Some(ah),
            },
            wgpu::Extent3d { width: aw, height: ah, depth_or_array_layers: 1 },
        );
    }

    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self::new_with_estimator(device, queue, None)
    }

    pub fn new_with_estimator(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        depth_estimator: Option<Box<dyn DepthEstimator>>,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("WireframeDepth"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fx_wireframe_depth.wgsl").into(),
            ),
        });

        let bgl = make_bind_group_layout(device);

        // Uniform buffer (80 bytes)
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("WireframeDepth uniforms"),
            size: std::mem::size_of::<WireframeDepthUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Sampler (Bilinear, Clamp)
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("WireframeDepth sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        // Dummy textures
        let dummy_rgba8 = create_1x1_texture(device, queue, wgpu::TextureFormat::Rgba8Unorm, "dummy_rgba8");
        let dummy_rgba8_view = dummy_rgba8.create_view(&wgpu::TextureViewDescriptor::default());
        let dummy_rgba16f = create_1x1_texture(device, queue, wgpu::TextureFormat::Rgba16Float, "dummy_rgba16f");
        let dummy_rgba16f_view = dummy_rgba16f.create_view(&wgpu::TextureViewDescriptor::default());
        let dummy_rgba32f = create_1x1_texture(device, queue, wgpu::TextureFormat::Rgba32Float, "dummy_rgba32f");
        let dummy_rgba32f_view = dummy_rgba32f.create_view(&wgpu::TextureViewDescriptor::default());

        // Per-pass output formats:
        // Pass 0  (Analysis)           → Rgba8Unorm  (ARGB32)
        // Pass 1  (HeuristicDepth)     → Rgba16Float (ARGBHalf)
        // Pass 2  (WireMask)           → Rgba8Unorm  (ARGB32)
        // Pass 3  (UpdateHistory)      → Rgba8Unorm  (ARGB32)
        // Pass 4  (Composite)          → Rgba16Float (main chain format)
        // Pass 5  (DnnDepthPost)       → Rgba16Float
        // Pass 6  (FlowEstimate)       → Rgba16Float
        // Pass 7  (FlowAdvectCoord)    → Rgba16Float
        // Pass 8  (InitMeshCoord)      → Rgba16Float
        // Pass 9  (MeshRegularize)     → Rgba16Float
        // Pass 10 (MeshCellAffine)     → Rgba16Float
        // Pass 11 (SemanticMask)       → Rgba16Float
        // Pass 12 (MeshFaceWarp)       → Rgba16Float
        // Pass 13 (SurfaceCacheUpdate) → Rgba16Float
        // Pass 14 (FlowHygiene)        → Rgba16Float
        let pass_formats: [wgpu::TextureFormat; PASS_COUNT] = [
            wgpu::TextureFormat::Rgba8Unorm,   // 0
            wgpu::TextureFormat::Rgba16Float,  // 1
            wgpu::TextureFormat::Rgba8Unorm,   // 2
            wgpu::TextureFormat::Rgba8Unorm,   // 3
            wgpu::TextureFormat::Rgba16Float,  // 4 (composite → main chain)
            wgpu::TextureFormat::Rgba16Float,  // 5
            wgpu::TextureFormat::Rgba16Float,  // 6
            wgpu::TextureFormat::Rgba16Float,  // 7
            wgpu::TextureFormat::Rgba16Float,  // 8
            wgpu::TextureFormat::Rgba16Float,  // 9
            wgpu::TextureFormat::Rgba16Float,  // 10
            wgpu::TextureFormat::Rgba16Float,  // 11
            wgpu::TextureFormat::Rgba16Float,  // 12
            wgpu::TextureFormat::Rgba16Float,  // 13
            wgpu::TextureFormat::Rgba16Float,  // 14
        ];
        let fs_entries: [&str; PASS_COUNT] = [
            "fs_pass0", "fs_pass1", "fs_pass2", "fs_pass3", "fs_pass4",
            "fs_pass5", "fs_pass6", "fs_pass7", "fs_pass8", "fs_pass9",
            "fs_pass10", "fs_pass11", "fs_pass12", "fs_pass13", "fs_pass14",
        ];
        let labels: [&str; PASS_COUNT] = [
            "WD Pass0 Analysis",       "WD Pass1 HeuristicDepth",
            "WD Pass2 WireMask",       "WD Pass3 UpdateHistory",
            "WD Pass4 Composite",      "WD Pass5 DnnDepthPost",
            "WD Pass6 FlowEstimate",   "WD Pass7 FlowAdvectCoord",
            "WD Pass8 InitMeshCoord",  "WD Pass9 MeshRegularize",
            "WD Pass10 MeshCellAffine","WD Pass11 SemanticMask",
            "WD Pass12 MeshFaceWarp",  "WD Pass13 SurfaceCacheUpdate",
            "WD Pass14 FlowHygiene",
        ];

        // Build all 15 pipelines
        let pipelines: [wgpu::RenderPipeline; PASS_COUNT] = std::array::from_fn(|i| {
            make_pipeline(device, &shader, &bgl, fs_entries[i], pass_formats[i], labels[i])
        });

        Self {
            pipelines,
            bind_group_layout: bgl,
            sampler,
            uniform_buffer,
            dummy_rgba8,
            dummy_rgba8_view,
            dummy_rgba16f,
            dummy_rgba16f_view,
            dummy_rgba32f,
            dummy_rgba32f_view,
            owner_states: HashMap::new(),
            width: 1920,
            height: 1080,
            depth_estimator,
            dnn_backend_initialized: false,
            dnn_backend_available: false,
            dnn_next_retry_frame: 0,
            warned_missing_dnn: false,
            dnn_subject_api_available: true,
            frame_count: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// PostProcessEffect + StatefulEffect impls
// ---------------------------------------------------------------------------

impl PostProcessEffect for WireframeDepthFX {
    fn effect_type(&self) -> EffectType {
        EffectType::WireframeDepth
    }

    // WireframeDepthFX.cs line 279-361 — Apply()
    fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        let amount = fx.param_values.get(0).copied().unwrap_or(0.0);
        if amount <= 0.0 { return; }

        // WireframeDepthFX.cs line 284-288 — read params
        let wire_scale = fx.param_values.get(9).copied().unwrap_or(1.0).clamp(0.5, 1.0);
        let mesh_rate  = (fx.param_values.get(10).copied().unwrap_or(1.0).round() as i32).clamp(1, 4);
        let native_flow_enabled = fx.param_values.get(11).copied().unwrap_or(1.0).round() as i32 > 0;
        let flow_lock_enabled   = fx.param_values.get(12).copied().unwrap_or(1.0).round() as i32 > 0;
        let face_warp_enabled   = fx.param_values.get(13).copied().unwrap_or(1.0).round() as i32 > 0;

        // WireframeDepthFX.cs line 291-300 — read remaining params
        let density         = fx.param_values.get(1).copied().unwrap_or(260.0);
        let line_width      = fx.param_values.get(2).copied().unwrap_or(1.335);
        let depth_scale     = fx.param_values.get(3).copied().unwrap_or(1.35);
        let temporal_smooth = fx.param_values.get(4).copied().unwrap_or(0.90);
        let persistence     = fx.param_values.get(5).copied().unwrap_or(0.82);
        let depth_mode      = if fx.param_values.get(6).copied().unwrap_or(0.0).round() as i32 > 0 {
            DepthSourceMode::Dnn
        } else {
            DepthSourceMode::Heuristic
        };
        let subject_isolation = fx.param_values.get(7).copied().unwrap_or(0.52).clamp(0.0, 1.0);
        let blend_mode = fx.param_values.get(8).copied().unwrap_or(6.0).clamp(0.0, 6.0);

        self.frame_count += 1;

        // GetOrCreateOwner (may rebuild wire RT if scale changed)
        // We need source texture dimensions for analysis size; use ctx.width/height
        self.width  = ctx.width;
        self.height = ctx.height;

        {
            // Run get_or_create_owner — creates state if missing, updates wire size
            let wire_w = ((ctx.width as f32 * wire_scale).round() as i32).max(64);
            let wire_h = ((ctx.height as f32 * wire_scale).round() as i32).max(36);

            if let Some(state) = self.owner_states.get_mut(&ctx.owner_key) {
                if state.wire_width != wire_w || state.wire_height != wire_h {
                    state.line_history_tex = RenderTarget::new(
                        device,
                        wire_w as u32, wire_h as u32,
                        wgpu::TextureFormat::Rgba8Unorm,
                        &format!("WireframeDepthHistory_{}", ctx.owner_key),
                    );
                    clear_rt(encoder, &state.line_history_tex.view);
                    state.wire_width  = wire_w;
                    state.wire_height = wire_h;
                }
            } else {
                let scale = 1f32.min(MAX_ANALYSIS_DIM as f32 / ctx.width.max(ctx.height) as f32);
                let analysis_width  = ((ctx.width  as f32 * scale).round() as i32).max(64);
                let analysis_height = ((ctx.height as f32 * scale).round() as i32).max(36);
                let mut state = OwnerState::new(
                    device, analysis_width, analysis_height, wire_w, wire_h, ctx.owner_key,
                );
                clear_rt(encoder, &state.previous_analysis_tex.view);
                clear_rt(encoder, &state.depth_tex.view);
                clear_rt(encoder, &state.line_history_tex.view);
                clear_rt(encoder, &state.flow_tex.view);
                clear_rt(encoder, &state.semantic_tex.view);
                clear_rt(encoder, &state.surface_cache_tex.view);
                clear_rt(encoder, &state.dnn_input_tex.view);
                state.last_native_request_frame  = -1024;
                state.last_subject_request_frame = -1024;
                state.last_mesh_update_frame     = -1024;
                self.owner_states.insert(ctx.owner_key, state);

                // InitializeMeshCoord on fresh state
                let base_u = WireframeDepthUniforms {
                    amount: 0.0, grid_density: density, line_width, depth_scale,
                    temporal_smooth, persistence, flow_lock_strength: 0.0,
                    mesh_regularize: 0.0, cell_affine_strength: 0.0, face_warp_strength: 0.0,
                    surface_persistence: 0.0, wire_taa: 0.0, subject_isolation,
                    blend_mode, main_texel_x: 1.0 / analysis_width as f32,
                    main_texel_y: 1.0 / analysis_height as f32,
                    depth_texel_x: 1.0 / analysis_width as f32,
                    depth_texel_y: 1.0 / analysis_height as f32,
                    _pad0: 0.0, _pad1: 0.0,
                };
                let st = self.owner_states.get(&ctx.owner_key).unwrap();
                self.initialize_mesh_coord(device, encoder, st, base_u);
            }
        }

        // Build base uniforms (values that many passes share)
        let state = self.owner_states.get(&ctx.owner_key).unwrap();
        let analysis_width  = state.analysis_width;
        let analysis_height = state.analysis_height;
        let wire_width  = state.wire_width as u32;
        let wire_height = state.wire_height as u32;

        let wire_taa = 0.48f32 + (0.92 - 0.48) * temporal_smooth.clamp(0.0, 1.0);

        let base_uniforms = WireframeDepthUniforms {
            amount,
            grid_density: density,
            line_width,
            depth_scale,
            temporal_smooth,
            persistence,
            flow_lock_strength: 0.0, // set per-pass
            mesh_regularize:    0.0, // set per-pass
            cell_affine_strength: 0.0, // set per-pass
            face_warp_strength: 0.0,   // set per-pass
            surface_persistence: 0.0,  // set per-pass
            wire_taa,
            subject_isolation,
            blend_mode,
            main_texel_x: 1.0 / ctx.width as f32,
            main_texel_y: 1.0 / ctx.height as f32,
            depth_texel_x: 1.0 / analysis_width as f32,
            depth_texel_y: 1.0 / analysis_height as f32,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        // EstimateDepth — runs all sub-passes (analysis, depth, flow lock, mesh pipeline)
        self.estimate_depth(
            device, queue, encoder,
            source,
            ctx.width, ctx.height,
            ctx.owner_key,
            temporal_smooth,
            depth_mode,
            subject_isolation,
            mesh_rate,
            native_flow_enabled,
            flow_lock_enabled,
            face_warp_enabled,
            base_uniforms,
        );

        // --- After EstimateDepth, run the wire mask + history + composite ---

        // Upload DNN subject texture if dirty (dnnSubjectDirty check)
        if self.owner_states.get(&ctx.owner_key).map(|s| s.dnn_subject_dirty).unwrap_or(false) {
            let state = self.owner_states.get_mut(&ctx.owner_key).unwrap();
            Self::upload_dnn_subject_texture(queue, state);
        }

        let state = self.owner_states.get(&ctx.owner_key).unwrap();
        let d   = &self.dummy_rgba8_view;
        let d16 = &self.dummy_rgba16f_view;

        // Temp lineMask RT (ARGB32 → Rgba8Unorm, wire size)
        let line_mask = RenderTarget::new(
            device, wire_width, wire_height,
            wgpu::TextureFormat::Rgba8Unorm, "WireframeDepth_lineMask");

        // Determine subject mask texture view
        let dnn_subject_view: &wgpu::TextureView =
            if depth_mode == DepthSourceMode::Dnn
                && state.dnn_has_subject_mask
            {
                &state.dnn_subject_texture.view
            } else {
                d
            };

        // Pass 2: WireMask — reads depth_tex, mesh_coord_tex, semantic_tex,
        //         surface_cache_tex, subject_mask_tex
        let mut u_wire = base_uniforms;
        u_wire.main_texel_x  = 1.0 / wire_width as f32;
        u_wire.main_texel_y  = 1.0 / wire_height as f32;
        u_wire.depth_texel_x = 1.0 / analysis_width as f32;
        u_wire.depth_texel_y = 1.0 / analysis_height as f32;
        self.run_pass(
            device, encoder, PASS_WIREFRAME_MASK,
            &PassBind {
                main_tex: source,
                prev_analysis_tex: d,
                prev_depth_tex: d16,
                depth_tex: &state.depth_tex.view,
                history_tex: d,
                flow_tex: d16,
                mesh_coord_tex: &state.mesh_coord_tex.view,
                prev_mesh_coord_tex: d16,
                semantic_tex: &state.semantic_tex.view,
                surface_cache_tex: &state.surface_cache_tex.view,
                prev_surface_cache_tex: d16,
                subject_mask_tex: dnn_subject_view,
            },
            &line_mask.view,
            u_wire,
            wgpu::TextureFormat::Rgba8Unorm,
        );

        // Pass 3: UpdateHistory (never read+write same RT → write to historyNext then copy)
        let history_next = RenderTarget::new(
            device, wire_width, wire_height,
            wgpu::TextureFormat::Rgba8Unorm, "WireframeDepth_historyNext");
        let mut u_hist = base_uniforms;
        u_hist.main_texel_x = 1.0 / wire_width as f32;
        u_hist.main_texel_y = 1.0 / wire_height as f32;
        u_hist.wire_taa     = wire_taa;

        let state = self.owner_states.get(&ctx.owner_key).unwrap();
        self.run_pass(
            device, encoder, PASS_UPDATE_HISTORY,
            &PassBind {
                main_tex: &line_mask.view,
                prev_analysis_tex: d,
                prev_depth_tex: d16,
                depth_tex: d16,
                history_tex: &state.line_history_tex.view,
                flow_tex: d16,
                mesh_coord_tex: d16,
                prev_mesh_coord_tex: d16,
                semantic_tex: d16,
                surface_cache_tex: &state.surface_cache_tex.view,
                prev_surface_cache_tex: d16,
                subject_mask_tex: d,
            },
            &history_next.view,
            u_hist,
            wgpu::TextureFormat::Rgba8Unorm,
        );

        // Graphics.Blit(historyNext, state.lineHistoryTex)
        {
            let state = self.owner_states.get(&ctx.owner_key).unwrap();
            Self::copy_texture(
                encoder,
                &history_next.texture,
                &state.line_history_tex.texture,
                wire_width,
                wire_height,
            );
        }

        // Pass 4: Composite — blit source + lineHistoryTex → target
        let state = self.owner_states.get(&ctx.owner_key).unwrap();
        let mut u_comp = base_uniforms;
        u_comp.main_texel_x = 1.0 / ctx.width as f32;
        u_comp.main_texel_y = 1.0 / ctx.height as f32;
        self.run_pass(
            device, encoder, PASS_COMPOSITE,
            &PassBind {
                main_tex: source,
                prev_analysis_tex: d,
                prev_depth_tex: d16,
                depth_tex: d16,
                history_tex: &state.line_history_tex.view,
                flow_tex: d16,
                mesh_coord_tex: d16,
                prev_mesh_coord_tex: d16,
                semantic_tex: d16,
                surface_cache_tex: d16,
                prev_surface_cache_tex: d16,
                subject_mask_tex: d,
            },
            target,
            u_comp,
            wgpu::TextureFormat::Rgba16Float,
        );
    }

    // WireframeDepthFX.cs line 915-919 — ClearState() (all owners)
    fn clear_state(&mut self) {
        // We can't easily call clear_owner_state here without device/queue.
        // Zero out flags and reset timers for each owner.
        for state in self.owner_states.values_mut() {
            state.dnn_readback_pending    = false;
            state.dnn_has_depth           = false;
            state.dnn_depth_dirty         = false;
            state.dnn_has_subject_mask    = false;
            state.dnn_subject_dirty       = false;
            state.has_prev_native_frame   = false;
            state.native_flow_has_data    = false;
            state.native_flow_dirty       = false;
            state.native_flow_ready       = false;
            state.native_request_wants_flow    = false;
            state.native_request_wants_depth   = false;
            state.native_request_wants_subject = false;
            state.latest_cut_score        = 0.0;
            state.last_subject_request_frame = -1024;
            state.last_mesh_update_frame     = -1024;
            for v in state.dnn_subject_history_buffer.iter_mut() { *v = 0.0; }
        }
    }

    fn resize(&mut self, _device: &wgpu::Device, width: u32, height: u32) {
        self.width  = width;
        self.height = height;
        // Per-owner state is rebuilt lazily in get_or_create_owner on next apply().
        self.owner_states.clear();
    }
}

impl StatefulEffect for WireframeDepthFX {
    // WireframeDepthFX.cs line 921-925 — ClearState(ownerKey)
    fn clear_state_for_owner(&mut self, owner_key: i64) {
        if let Some(state) = self.owner_states.get_mut(&owner_key) {
            state.dnn_readback_pending    = false;
            state.dnn_has_depth           = false;
            state.dnn_depth_dirty         = false;
            state.dnn_has_subject_mask    = false;
            state.dnn_subject_dirty       = false;
            state.has_prev_native_frame   = false;
            state.native_flow_has_data    = false;
            state.native_flow_dirty       = false;
            state.native_flow_ready       = false;
            state.native_request_wants_flow    = false;
            state.native_request_wants_depth   = false;
            state.native_request_wants_subject = false;
            state.latest_cut_score        = 0.0;
            state.last_subject_request_frame = -1024;
            state.last_mesh_update_frame     = -1024;
            for v in state.dnn_subject_history_buffer.iter_mut() { *v = 0.0; }
        }
    }

    // WireframeDepthFX.cs line 981-988 — CleanupOwner(ownerKey)
    fn cleanup_owner(&mut self, owner_key: i64) {
        self.owner_states.remove(&owner_key);
    }

    // WireframeDepthFX.cs line 990-996 — CleanupAllOwners()
    fn cleanup_all_owners(&mut self, _device: &wgpu::Device) {
        self.owner_states.clear();
        self.warned_missing_dnn = false;
    }
}
