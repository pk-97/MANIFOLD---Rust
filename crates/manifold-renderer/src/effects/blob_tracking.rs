// Mechanical port of BlobTrackingFX.cs + BlobTrackingEffect.shader.
// Same logic, same variables, same constants, same edge cases.
//
// Unity GPU readback (AsyncGPUReadback) maps to poll-based ReadbackRequest.
// The "frame" counter maps to an app-managed frame_count in EffectContext.
// Unity's OnReadbackComplete callback maps to try_read() polled at apply() start.

use std::collections::HashMap;
use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::background_worker::BackgroundWorker;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::gpu_readback::ReadbackRequest;
use crate::render_target::RenderTarget;

// Request/response types for the background blob detection worker.
struct BlobRequest {
    pixel_buffer: Vec<u8>,
    threshold: f32,
    sensitivity: f32,
}

struct BlobResponse {
    blob_data: Vec<f32>,  // MAX_BLOBS * 4: [x, y, w, h] per blob
    blob_count: i32,
}

// BlobTrackingFX.cs line 14-17 (tuned up from Unity: 320x180 @ every-3-frames)
// M4 Max unified memory makes per-frame readback essentially free.
const READBACK_WIDTH: u32 = 640;
const READBACK_HEIGHT: u32 = 360;
const MAX_BLOBS: usize = 16;
const READBACK_INTERVAL_FRAMES: i64 = 1;

// BlobTrackingFX.cs line 35
const MATCH_RADIUS_SQ: f32 = 0.08;
const SIZE_SMOOTH_FACTOR: f32 = 0.85;

// Connection distance threshold is now param 4 ("Connect", 0.0–1.0, default 0.35).
// Squared before use in compute_connections().

// BlobTrackingFX.cs line 39-46 — TrackedBlob
#[derive(Clone, Copy, Default)]
struct TrackedBlob {
    smooth_pos: [f32; 2],
    smooth_size: [f32; 2],
    raw_pos: [f32; 2],
    raw_size: [f32; 2],
    matched: bool,
}

// BlobTrackingFX.cs line 48-68 — OwnerState
struct OwnerState {
    downsample_rt: RenderTarget,
    readback: ReadbackRequest,
    has_blob_data: bool,
    pixel_buffer: Vec<u8>,            // new byte[READBACK_WIDTH * READBACK_HEIGHT * 4]
    native_blob_output: Vec<f32>,     // new float[MAX_BLOBS * 4]
    blob_data_for_shader: Vec<[f32; 4]>, // Vector4[MAX_BLOBS]
    connection_lines: Vec<[f32; 4]>,     // Vector4[MAX_BLOBS]
    blob_count: i32,
    connection_count: i32,
    pending_threshold: f32,
    pending_sensitivity: f32,
    last_readback_frame: i64,
    // Temporal smoothing
    tracked: Vec<TrackedBlob>,        // new TrackedBlob[MAX_BLOBS]
    tracked_count: usize,
    has_new_detection: bool,
}

// Uniform struct for the overlay shader.
// 16-byte aligned: layout matches BlobTrackingEffect.shader uniforms.
//
// Amount        f32
// BlobCount     i32
// ConnectionCnt i32
// _pad0         f32
// Resolution    vec2<f32>  (width, height)
// TexelSize     vec2<f32>  (1/w, 1/h)
// BlobCenterSize array<vec4<f32>, 16>   → 16 * 16 = 256 bytes
// BlobConnects  array<vec4<f32>, 16>   → 16 * 16 = 256 bytes
// Total: 16 + 256 + 256 = 528 bytes (all 16-byte aligned)
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlobUniforms {
    amount: f32,
    blob_count: i32,
    connection_count: i32,
    _pad0: f32,
    resolution: [f32; 2],   // width, height
    texel_size: [f32; 2],   // 1/width, 1/height
    // _BlobCenterSize[MAX_BLOBS] — each vec4 is [cx, cy, sw, sh]
    blob_center_size: [[f32; 4]; 16],
    // _BlobConnections[MAX_BLOBS] — each vec4 is [ax, ay, bx, by]
    blob_connections: [[f32; 4]; 16],
}

// BlobTrackingFX.cs line 10 — BlobTrackingFX : IPostProcessEffect, IStatefulEffect
pub struct BlobTrackingFX {
    // Overlay shader pipeline (single pass — BlobTrackingEffect.shader has 1 pass)
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    // Point sampler for font atlas (filterMode = FilterMode.Point)
    point_sampler: wgpu::Sampler,
    // Simple blit pipeline for downsample pass (Graphics.Blit to 320x180)
    blit_pipeline: wgpu::RenderPipeline,
    blit_bgl: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    // BlobTrackingFX.cs line 24 — fontAtlas
    font_atlas: wgpu::Texture,
    font_atlas_view: wgpu::TextureView,
    // BlobTrackingFX.cs line 22 — nativeHandle (native blob detector)
    // Native processing runs on a background thread via BackgroundWorker.
    worker: Option<BackgroundWorker<BlobRequest, BlobResponse>>,
    // Track which owner submitted the in-flight worker request.
    pending_worker_owner: Option<i64>,
    // BlobTrackingFX.cs line 70 — ownerStates
    owner_states: HashMap<i64, OwnerState>,
}

impl BlobTrackingFX {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        // BlobTrackingFX.cs line 108-117 — try to create native detector
        // Plugin is created on the worker thread (single creation, no probe).
        // try_new returns None if the plugin isn't available.
        let worker = BackgroundWorker::try_new(|| {
            use manifold_native::blob_detector::BlobDetector;
            let detector = manifold_native::ffi::blob_ffi::FfiBlobDetector::new(MAX_BLOBS as i32)?;
            Some(move |req: BlobRequest| -> BlobResponse {
                let mut blob_data = vec![0f32; MAX_BLOBS * 4];
                let blob_count = detector.process(
                    &req.pixel_buffer,
                    READBACK_WIDTH as i32,
                    READBACK_HEIGHT as i32,
                    req.threshold,
                    req.sensitivity,
                    &mut blob_data,
                );
                BlobResponse { blob_data, blob_count }
            })
        });
        if worker.is_none() {
            log::warn!("[BlobTrackingFX] BlobDetector native plugin not found. \
                       Build it with Assets/Plugins/BlobDetector/build.sh");
        }

        let format = wgpu::TextureFormat::Rgba16Float;

        // ---- Overlay shader pipeline ----
        // BlobTrackingEffect.shader: 1 pass, reads _MainTex + _FontTex
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("BlobTracking"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fx_blob_tracking.wgsl").into(),
            ),
        });

        // Bindings: 0=uniforms, 1=_MainTex, 2=sampler, 3=_FontTex, 4=point_sampler
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("BlobTracking BGL"),
            entries: &[
                // binding 0: uniforms (BlobUniforms)
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
                // binding 1: _MainTex (source frame)
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
                // binding 2: bilinear sampler (for _MainTex)
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // binding 3: _FontTex
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // binding 4: point sampler (for _FontTex — filterMode = Point)
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("BlobTracking Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("BlobTracking Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
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
        });

        // ---- Simple blit pipeline (for downsample pass) ----
        // Unity: Graphics.Blit(buffer, state.downsampleRT)
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("BlobTracking Downsample"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });

        let blit_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("BlobTracking Blit BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let blit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("BlobTracking Blit Layout"),
            bind_group_layouts: &[&blit_bgl],
            immediate_size: 0,
        });

        // Downsample RT is Rgba8Unorm (Unity: RenderTextureUtil.Create → RGBA32)
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("BlobTracking Blit Pipeline"),
            layout: Some(&blit_layout),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
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
        });

        // ---- Samplers ----
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("BlobTracking Bilinear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // BlobTrackingFX.cs line 417: filterMode = FilterMode.Point
        let point_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("BlobTracking Point"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            // wgpu 28: mipmap_filter is MipmapFilterMode, not FilterMode
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            // BlobTrackingFX.cs line 418: wrapMode = TextureWrapMode.Clamp
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        // ---- Uniform buffer ----
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("BlobTracking Uniforms"),
            size: std::mem::size_of::<BlobUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ---- Font atlas ----
        // BlobTrackingFX.cs lines 385-442 — CreateFontAtlas()
        let (font_atlas, font_atlas_view) = create_font_atlas(device, queue);

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            point_sampler,
            blit_pipeline,
            blit_bgl,
            uniform_buffer,
            font_atlas,
            font_atlas_view,
            worker,
            pending_worker_owner: None,
            owner_states: HashMap::new(),
        }
    }

    // BlobTrackingFX.cs lines 72-95 — GetOrCreateOwner
    fn get_or_create_owner(&mut self, device: &wgpu::Device, owner_key: i64) -> &mut OwnerState {
        self.owner_states.entry(owner_key).or_insert_with(|| {
            // BlobTrackingFX.cs line 78: RenderTextureUtil.Create(320, 180, name)
            // Unity creates an RGBA32 RT; we use Rgba8Unorm for readback compatibility.
            let downsample_rt = RenderTarget::new(
                device,
                READBACK_WIDTH,
                READBACK_HEIGHT,
                wgpu::TextureFormat::Rgba8Unorm,
                &format!("BlobAnalysis_{owner_key}"),
            );

            // BlobTrackingFX.cs lines 80-84
            let pixel_buffer = vec![0u8; (READBACK_WIDTH * READBACK_HEIGHT * 4) as usize];
            let native_blob_output = vec![0f32; MAX_BLOBS * 4];
            let blob_data_for_shader = vec![[0f32; 4]; MAX_BLOBS];
            let connection_lines = vec![[0f32; 4]; MAX_BLOBS];
            // BlobTrackingFX.cs line 84: tracked = new TrackedBlob[MAX_BLOBS]
            let tracked = vec![TrackedBlob::default(); MAX_BLOBS];

            OwnerState {
                downsample_rt,
                readback: ReadbackRequest::new(),
                has_blob_data: false,
                pixel_buffer,
                native_blob_output,
                blob_data_for_shader,
                connection_lines,
                blob_count: 0,
                connection_count: 0,
                pending_threshold: 0.0,
                pending_sensitivity: 0.0,
                last_readback_frame: 0,
                tracked,
                tracked_count: 0,
                has_new_detection: false,
            }
        })
    }

    // BlobTrackingFX.cs lines 184-256 — OnReadbackComplete
    // Split into two non-blocking phases:
    //   1. Poll worker for completed blob detection result
    //   2. Poll GPU readback for new pixel data → submit to worker
    fn poll_readback(&mut self, device: &wgpu::Device, owner_key: i64) {
        // ── Phase 1: check if the background worker has a result ──
        if let Some(worker) = &mut self.worker {
            if let Some(response) = worker.try_recv() {
                // Route result to the owner that submitted it.
                let result_owner = self.pending_worker_owner.take().unwrap_or(owner_key);
                if let Some(state) = self.owner_states.get_mut(&result_owner) {
                    Self::apply_blob_response(state, &response);
                }
            }
        }

        // ── Phase 2: check for new pixel data from GPU readback ──
        let Some(state) = self.owner_states.get_mut(&owner_key) else { return };

        let pixels = match state.readback.try_read(device) {
            Some(p) => p,
            None => return,
        };

        // BlobTrackingFX.cs line 195: if (nativeHandle == IntPtr.Zero) return;
        let Some(worker) = &mut self.worker else { return };

        // Submit to background worker (non-blocking).
        worker.submit(BlobRequest {
            pixel_buffer: pixels,
            threshold: state.pending_threshold,
            sensitivity: state.pending_sensitivity,
        });
        self.pending_worker_owner = Some(owner_key);
    }

    // Apply a completed BlobResponse to OwnerState.
    // BlobTrackingFX.cs lines 204-252 — greedy nearest-neighbour matching
    fn apply_blob_response(state: &mut OwnerState, response: &BlobResponse) {
        // Copy raw blob output into state for matching
        let copy_len = response.blob_data.len().min(state.native_blob_output.len());
        state.native_blob_output[..copy_len].copy_from_slice(&response.blob_data[..copy_len]);

        // Mark all existing tracked blobs as unmatched
        for i in 0..state.tracked_count {
            state.tracked[i].matched = false;
        }

        // For each new detection, find closest unmatched tracked blob
        for d in 0..response.blob_count as usize {
            let dx = state.native_blob_output[d * 4 + 0];
            // The C++ plugin outputs Y in Unity UV convention (v=0 at bottom).
            // Keep as-is: the overlay shader uses a Y-flipped draw_uv that matches
            // Unity's convention, so blob positions flow through unchanged.
            let dy = state.native_blob_output[d * 4 + 1];
            let dw = state.native_blob_output[d * 4 + 2];
            let dh = state.native_blob_output[d * 4 + 3];

            let mut best_dist_sq = MATCH_RADIUS_SQ;
            let mut best_idx: i32 = -1;

            for t in 0..state.tracked_count {
                if state.tracked[t].matched { continue; }
                let ex = state.tracked[t].raw_pos[0] - dx;
                let ey = state.tracked[t].raw_pos[1] - dy;
                let dist_sq = ex * ex + ey * ey;
                if dist_sq < best_dist_sq {
                    best_dist_sq = dist_sq;
                    best_idx = t as i32;
                }
            }

            if best_idx >= 0 {
                // Update existing tracked blob target
                let idx = best_idx as usize;
                state.tracked[idx].raw_pos = [dx, dy];
                state.tracked[idx].raw_size = [dw, dh];
                state.tracked[idx].matched = true;
            } else if state.tracked_count < MAX_BLOBS {
                // New blob — initialize at detection position
                let idx = state.tracked_count;
                state.tracked_count += 1;
                state.tracked[idx] = TrackedBlob {
                    smooth_pos: [dx, dy],
                    smooth_size: [dw, dh],
                    raw_pos: [dx, dy],
                    raw_size: [dw, dh],
                    matched: true,
                };
            }
        }

        state.has_new_detection = true;
        state.has_blob_data = true;
    }

    // BlobTrackingFX.cs lines 258-291 — UpdateSmoothing (static method)
    fn update_smoothing(state: &mut OwnerState, smoothing: f32, dt: f32) {
        // BlobTrackingFX.cs line 263: Mathf.Lerp(60f, 2f, smoothing)
        // Mathf.Lerp clamps t to [0,1]
        let lerp_speed = 60.0 + (2.0 - 60.0) * smoothing.clamp(0.0, 1.0);
        let pos_alpha = 1.0 - (-lerp_speed * dt).exp();
        let size_alpha = 1.0 - (-lerp_speed * SIZE_SMOOTH_FACTOR * dt).exp();

        // BlobTrackingFX.cs lines 268-281 — remove unmatched blobs on new detection
        if state.has_new_detection {
            let mut write = 0usize;
            for read in 0..state.tracked_count {
                if state.tracked[read].matched {
                    if write != read {
                        state.tracked[write] = state.tracked[read];
                    }
                    write += 1;
                }
            }
            state.tracked_count = write;
            state.has_new_detection = false;
        }

        // BlobTrackingFX.cs lines 283-290 — lerp positions and sizes
        for i in 0..state.tracked_count {
            let t = state.tracked[i];
            // Vector2.Lerp(a, b, t) = a + (b-a)*clamp(t,0,1) — but alpha is already [0,1]
            state.tracked[i].smooth_pos = [
                t.smooth_pos[0] + (t.raw_pos[0] - t.smooth_pos[0]) * pos_alpha,
                t.smooth_pos[1] + (t.raw_pos[1] - t.smooth_pos[1]) * pos_alpha,
            ];
            state.tracked[i].smooth_size = [
                t.smooth_size[0] + (t.raw_size[0] - t.smooth_size[0]) * size_alpha,
                t.smooth_size[1] + (t.raw_size[1] - t.smooth_size[1]) * size_alpha,
            ];
        }
    }

    // BlobTrackingFX.cs lines 293-327 — ComputeConnections (static method)
    // connect_dist: param 4 "Connect" (0.0–1.0), squared before comparison.
    fn compute_connections(state: &mut OwnerState, connect_dist: f32) {
        state.connection_count = 0;
        let threshold_sq = connect_dist * connect_dist;

        let mut c = 0usize;
        let mut i = 0usize;
        while i < state.blob_count as usize && c < MAX_BLOBS {
            let mut best_dist = f32::MAX;
            let mut best_j: i32 = -1;
            let a = state.blob_data_for_shader[i];

            let mut j = i + 1;
            while j < state.blob_count as usize {
                let b = state.blob_data_for_shader[j];
                let dx = a[0] - b[0];
                let dy = a[1] - b[1];
                let dist = dx * dx + dy * dy;

                if dist < best_dist && dist < threshold_sq {
                    best_dist = dist;
                    best_j = j as i32;
                }
                j += 1;
            }

            if best_j >= 0 {
                let b = state.blob_data_for_shader[best_j as usize];
                state.connection_lines[c] = [a[0], a[1], b[0], b[1]];
                c += 1;
            }
            i += 1;
        }

        state.connection_count = c as i32;

        // BlobTrackingFX.cs lines 325-326: zero out unused connection slots
        for i in c..MAX_BLOBS {
            state.connection_lines[i] = [0.0; 4];
        }
    }
}

// BlobTrackingFX.cs lines 385-442 — CreateFontAtlas()
// 5x7 glyphs, 16 cols x 2 rows = 80x14 texture, RGBA32, Point filter, Clamp
fn create_font_atlas(device: &wgpu::Device, queue: &wgpu::Queue) -> (wgpu::Texture, wgpu::TextureView) {
    const GW: usize = 5;
    const GH: usize = 7;
    const COLS: usize = 16;
    const ROWS: usize = 2;
    let tex_w = COLS * GW;
    let tex_h = ROWS * GH;

    // BlobTrackingFX.cs lines 391-414 — glyph bitmaps (IDENTICAL to Unity source)
    let glyphs: &[&[&str]] = &[
        &[".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###."], // 0
        &["..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###."], // 1
        &[".###.", "#...#", "....#", "..##.", ".#...", "#....", "#####"], // 2
        &[".###.", "#...#", "....#", "..##.", "....#", "#...#", ".###."], // 3
        &["...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#."], // 4
        &["#####", "#....", "####.", "....#", "....#", "#...#", ".###."], // 5
        &[".###.", "#....", "#....", "####.", "#...#", "#...#", ".###."], // 6
        &["#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#..."], // 7
        &[".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###."], // 8
        &[".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##.."], // 9
        &[".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"], // A
        &["####.", "#...#", "#...#", "####.", "#...#", "#...#", "####."], // B
        &[".###.", "#...#", "#....", "#....", "#....", "#...#", ".###."], // C
        &["####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####."], // D
        &["#####", "#....", "#....", "####.", "#....", "#....", "#####"], // E
        &["#####", "#....", "#....", "####.", "#....", "#....", "#...."], // F
        &["#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#"], // X
        &["#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#.."], // Y
        &[".....", ".....", ".....", ".....", ".....", ".....", "..#.."], // .
        &[".....", "..#..", "..#..", ".....", "..#..", "..#..", "....."], // :
        &["##..#", "##.#.", "..#..", "..#..", "..#..", ".#.##", "#..##"], // %
    ];

    // BlobTrackingFX.cs line 422: var pixels = new Color32[texW * texH]
    // All pixels start as transparent black (Color32 default = {0,0,0,0})
    let mut pixels = vec![[0u8; 4]; tex_w * tex_h];

    for (c, glyph) in glyphs.iter().enumerate() {
        let base_x = (c % COLS) * GW;
        let base_y = (c / COLS) * GH;
        for row in 0..GH {
            // BlobTrackingFX.cs line 429: int texY = baseY + (GH - 1 - row)
            let tex_y = base_y + (GH - 1 - row);
            let line = glyph[row];
            for col in 0..GW {
                if col < line.len() && line.as_bytes()[col] == b'#' {
                    // BlobTrackingFX.cs line 434: new Color32(255, 255, 255, 255)
                    pixels[tex_y * tex_w + base_x + col] = [255, 255, 255, 255];
                }
            }
        }
    }

    // BlobTrackingFX.cs line 416: new Texture2D(texW, texH, TextureFormat.RGBA32, false)
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("BlobTracking FontAtlas"),
        size: wgpu::Extent3d {
            width: tex_w as u32,
            height: tex_h as u32,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // RGBA32 → Rgba8Unorm
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    // Upload pixel data
    // wgpu 28: ImageCopyTexture → TexelCopyTextureInfo, ImageDataLayout → TexelCopyBufferLayout
    let flat: Vec<u8> = pixels.iter().flat_map(|p| p.iter().copied()).collect();
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &flat,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some((tex_w * 4) as u32),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: tex_w as u32,
            height: tex_h as u32,
            depth_or_array_layers: 1,
        },
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

// Minimal passthrough blit shader — used for the downsample pass.
// Unity: Graphics.Blit(buffer, state.downsampleRT) — bilinear blit.
const BLIT_SHADER: &str = r#"
@group(0) @binding(0) var source_tex: texture_2d<f32>;
@group(0) @binding(1) var tex_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(source_tex, tex_sampler, in.uv);
}
"#;

impl PostProcessEffect for BlobTrackingFX {
    fn effect_type(&self) -> EffectType {
        EffectType::BlobTracking
    }

    // BlobTrackingFX.cs line 127: if (amount <= 0f || material == null) return;
    fn should_skip(&self, fx: &EffectInstance) -> bool {
        fx.param_values.first().copied().unwrap_or(0.0) <= 0.0
    }

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
        // BlobTrackingFX.cs line 126-127
        let amount = fx.param_values.first().copied().unwrap_or(0.0);
        if amount <= 0.0 { return; }

        // BlobTrackingFX.cs lines 129-131
        let threshold    = fx.param_values.get(1).copied().unwrap_or(0.65);
        let sensitivity  = fx.param_values.get(2).copied().unwrap_or(0.85);
        let smoothing    = fx.param_values.get(3).copied().unwrap_or(0.7);
        let connect_dist = fx.param_values.get(4).copied().unwrap_or(0.35);

        // BlobTrackingFX.cs line 133
        self.get_or_create_owner(device, ctx.owner_key);

        // ---- Phase 0: poll any pending readback from a previous frame ----
        // Unity: OnReadbackComplete fires asynchronously; we poll here instead.
        self.poll_readback(device, ctx.owner_key);

        let state = self.owner_states.get_mut(&ctx.owner_key).unwrap();

        // ---- Phase 1: Blit to downsample RT and request readback (throttled) ----
        // BlobTrackingFX.cs lines 136-148
        let frame = ctx.frame_count;
        if !state.readback.is_pending()
            && frame - state.last_readback_frame >= READBACK_INTERVAL_FRAMES
        {
            // Graphics.Blit(buffer, state.downsampleRT) — encode blit pass
            let blit_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("BlobTracking Downsample BG"),
                layout: &self.blit_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("BlobTracking Downsample"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &state.downsample_rt.view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.blit_pipeline);
                pass.set_bind_group(0, &blit_bg, &[]);
                pass.draw(0..3, 0..1);
            }

            // AsyncGPUReadback.Request(state.downsampleRT, ...) — submit readback
            state.readback.submit(
                device,
                encoder,
                &state.downsample_rt.texture,
                READBACK_WIDTH,
                READBACK_HEIGHT,
            );
            state.pending_threshold = threshold;
            state.pending_sensitivity = sensitivity;
            state.last_readback_frame = frame;
        }

        // ---- Phase 2: Per-frame temporal smoothing ----
        // BlobTrackingFX.cs lines 150-152
        if state.has_blob_data {
            Self::update_smoothing(state, smoothing, ctx.dt);
        }

        // ---- Phase 3: Render overlay with smoothed blob data ----
        // BlobTrackingFX.cs lines 154-155: Unity returns early here because its swap
        // is inside Apply(). In Rust the effect chain swaps unconditionally after apply(),
        // so we must always write to target. With 0 blobs the shader is a near-passthrough
        // (only adds a subtle scanline weighted by amount).

        // Build shader data from smoothed tracked blobs
        // BlobTrackingFX.cs lines 157-166
        for i in 0..state.tracked_count {
            let t = state.tracked[i];
            state.blob_data_for_shader[i] = [
                t.smooth_pos[0], t.smooth_pos[1],
                t.smooth_size[0], t.smooth_size[1],
            ];
        }
        for i in state.tracked_count..MAX_BLOBS {
            state.blob_data_for_shader[i] = [0.0; 4];
        }

        state.blob_count = state.tracked_count as i32;
        Self::compute_connections(state, connect_dist);

        // Build uniform buffer
        // BlobTrackingFX.cs lines 171-176
        let mut blob_center_size = [[0f32; 4]; 16];
        let mut blob_connections_arr = [[0f32; 4]; 16];
        blob_center_size[..MAX_BLOBS].copy_from_slice(&state.blob_data_for_shader[..MAX_BLOBS]);
        blob_connections_arr[..MAX_BLOBS].copy_from_slice(&state.connection_lines[..MAX_BLOBS]);

        let uniforms = BlobUniforms {
            amount,
            blob_count: state.blob_count,
            connection_count: state.connection_count,
            _pad0: 0.0,
            resolution: [ctx.width as f32, ctx.height as f32],
            texel_size: [1.0 / ctx.width as f32, 1.0 / ctx.height as f32],
            blob_center_size,
            blob_connections: blob_connections_arr,
        };

        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        // BlobTrackingFX.cs lines 178-181 — Blit with overlay shader
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("BlobTracking BG"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&self.font_atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.point_sampler),
                },
            ],
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("BlobTracking Overlay"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    // BlobTrackingFX.cs lines 329-333 — ClearState() (all owners)
    fn clear_state(&mut self) {
        for state in self.owner_states.values_mut() {
            clear_owner_state(state);
        }
    }

    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {
        // BlobTrackingFX.cs lines 366-368:
        // "Downsample RT is fixed size, no resize needed"
    }
}

impl StatefulEffect for BlobTrackingFX {
    // BlobTrackingFX.cs lines 335-339 — ClearState(int ownerKey)
    fn clear_state_for_owner(&mut self, owner_key: i64) {
        if let Some(state) = self.owner_states.get_mut(&owner_key) {
            clear_owner_state(state);
        }
    }

    // BlobTrackingFX.cs lines 350-357 — CleanupOwner
    fn cleanup_owner(&mut self, owner_key: i64) {
        // RenderTextureUtil.Release drops the RT; removal from map drops the struct.
        self.owner_states.remove(&owner_key);
    }

    // BlobTrackingFX.cs lines 359-364 — CleanupAllOwners
    fn cleanup_all_owners(&mut self, _device: &wgpu::Device) {
        self.owner_states.clear();
    }
}

// BlobTrackingFX.cs lines 341-348 — ClearOwnerState (static helper)
fn clear_owner_state(state: &mut OwnerState) {
    state.has_blob_data = false;
    state.blob_count = 0;
    state.connection_count = 0;
    state.tracked_count = 0;
    state.has_new_detection = false;
}
