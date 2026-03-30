// Mechanical port of BlobTrackingFX.cs + BlobTrackingEffect.shader.
// Same logic, same variables, same constants, same edge cases.
//
// Unity GPU readback (AsyncGPUReadback) maps to poll-based ReadbackRequest.
// The "frame" counter maps to an app-managed frame_count in EffectContext.
// Unity's OnReadbackComplete callback maps to try_read() polled at apply() start.

use crate::background_worker::BackgroundWorker;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::gpu_encoder::GpuEncoder;
use crate::gpu_readback::ReadbackRequest;
use crate::render_target::RenderTarget;
use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use manifold_gpu::{
    GpuBinding, GpuComputePipeline, GpuDevice, GpuFilterMode, GpuSampler, GpuSamplerDesc,
    GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};

// Request/response types for the background blob detection worker.
struct BlobRequest {
    owner_key: i64,
    pixel_buffer: Vec<u8>,
    width: i32,
    height: i32,
    threshold: f32,
    sensitivity: f32,
}

struct BlobResponse {
    owner_key: i64,
    blob_data: Vec<f32>, // MAX_BLOBS * 4: [x, y, w, h] per blob
    blob_count: i32,
}

// Unity hardcodes 320x180, but that squashes vertical projects into landscape.
// Instead, preserve source aspect ratio with the same pixel budget (57,600 px).
const READBACK_PIXEL_BUDGET: u32 = 320 * 180; // 57,600
const MAX_BLOBS: usize = 8;
const READBACK_INTERVAL_FRAMES: i64 = 1;

/// Compute readback dimensions that preserve source aspect ratio.
/// Keeps total pixel count ≈ READBACK_PIXEL_BUDGET, rounds to multiples of 16
/// for compute workgroup alignment.
fn readback_dims(source_w: u32, source_h: u32) -> (u32, u32) {
    let aspect = source_w as f64 / source_h as f64;
    let h = (READBACK_PIXEL_BUDGET as f64 / aspect).sqrt();
    let w = h * aspect;
    // Round to multiples of 16 (compute workgroup size), clamp to at least 16
    let rw = ((w as u32).max(16) + 15) & !15;
    let rh = ((h as u32).max(16) + 15) & !15;
    (rw, rh)
}

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
    readback_w: u32,
    readback_h: u32,
    has_blob_data: bool,
    _pixel_buffer: Vec<u8>,
    native_blob_output: Vec<f32>, // new float[MAX_BLOBS * 4]
    blob_data_for_shader: Vec<[f32; 4]>, // Vector4[MAX_BLOBS]
    connection_lines: Vec<[f32; 4]>, // Vector4[MAX_BLOBS]
    blob_count: i32,
    connection_count: i32,
    pending_threshold: f32,
    pending_sensitivity: f32,
    last_readback_frame: i64,
    // Temporal smoothing
    tracked: Vec<TrackedBlob>, // new TrackedBlob[MAX_BLOBS]
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
    resolution: [f32; 2], // width, height
    texel_size: [f32; 2], // 1/width, 1/height
    // _BlobCenterSize[MAX_BLOBS] — each vec4 is [cx, cy, sw, sh]
    blob_center_size: [[f32; 4]; MAX_BLOBS],
    // _BlobConnections[MAX_BLOBS] — each vec4 is [ax, ay, bx, by]
    blob_connections: [[f32; 4]; MAX_BLOBS],
}

const _: () = assert!(std::mem::size_of::<BlobUniforms>() == 288);

// BlobTrackingFX.cs line 10 — BlobTrackingFX : IPostProcessEffect, IStatefulEffect
pub struct BlobTrackingFX {
    // Compute pipeline for overlay pass (blob visualization).
    compute_overlay: GpuComputePipeline,
    // Compute pipeline for downsample pass (bilinear blit to readback size).
    compute_downsample: GpuComputePipeline,
    // Bilinear sampler for source texture.
    sampler: GpuSampler,
    // Point sampler for font atlas (filterMode = FilterMode.Point).
    point_sampler: GpuSampler,
    // BlobTrackingFX.cs line 24 — fontAtlas
    font_atlas: GpuTexture,
    // BlobTrackingFX.cs line 22 — nativeHandle (native blob detector)
    // Native processing runs on a background thread via BackgroundWorker.
    // Single worker shared across owners — guarded by is_busy() to prevent
    // drain-to-latest from discarding one owner's request in favour of another.
    worker: Option<BackgroundWorker<BlobRequest, BlobResponse>>,
    // BlobTrackingFX.cs line 70 — ownerStates
    owner_states: AHashMap<i64, OwnerState>,
}

unsafe impl Send for BlobTrackingFX {}

impl BlobTrackingFX {
    pub fn new(device: &GpuDevice) -> Self {
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
                    req.width,
                    req.height,
                    req.threshold,
                    req.sensitivity,
                    &mut blob_data,
                );
                BlobResponse {
                    owner_key: req.owner_key,
                    blob_data,
                    blob_count,
                }
            })
        });
        if worker.is_none() {
            log::warn!(
                "[BlobTrackingFX] BlobDetector native plugin not found. \
                 Build it with Assets/Plugins/BlobDetector/build.sh"
            );
        }

        // ---- Compute pipelines ----
        let compute_overlay = device.create_compute_pipeline(
            include_str!("shaders/fx_blob_tracking_compute.wgsl"),
            "cs_main",
            "BlobTracking Overlay",
        );
        let compute_downsample = device.create_compute_pipeline(
            DOWNSAMPLE_COMPUTE_SHADER,
            "cs_downsample",
            "BlobTracking Downsample",
        );

        // ---- Samplers ----
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        // BlobTrackingFX.cs line 417: filterMode = FilterMode.Point
        let point_sampler = device.create_sampler(&GpuSamplerDesc {
            min_filter: GpuFilterMode::Nearest,
            mag_filter: GpuFilterMode::Nearest,
            ..GpuSamplerDesc::default()
        });

        // ---- Font atlas ----
        // BlobTrackingFX.cs lines 385-442 — CreateFontAtlas()
        let font_atlas = create_font_atlas(device);

        Self {
            compute_overlay,
            compute_downsample,
            sampler,
            point_sampler,
            font_atlas,
            worker,
            owner_states: AHashMap::new(),
        }
    }

    // BlobTrackingFX.cs lines 72-95 — GetOrCreateOwner
    // Readback dimensions are computed from source aspect ratio to avoid distortion.
    fn get_or_create_owner(
        &mut self,
        device: &GpuDevice,
        pool: Option<&manifold_gpu::TexturePool>,
        owner_key: i64,
        source_w: u32,
        source_h: u32,
    ) -> &mut OwnerState {
        self.owner_states.entry(owner_key).or_insert_with(|| {
            let (rw, rh) = readback_dims(source_w, source_h);
            let downsample_rt = if let Some(p) = pool {
                RenderTarget::new_pooled(
                    p,
                    rw,
                    rh,
                    GpuTextureFormat::Rgba8Unorm,
                    &format!("BlobAnalysis_{owner_key}"),
                )
            } else {
                RenderTarget::new(
                    device,
                    rw,
                    rh,
                    GpuTextureFormat::Rgba8Unorm,
                    &format!("BlobAnalysis_{owner_key}"),
                )
            };

            let pixel_buffer = vec![0u8; (rw * rh * 4) as usize];
            let native_blob_output = vec![0f32; MAX_BLOBS * 4];
            let blob_data_for_shader = vec![[0f32; 4]; MAX_BLOBS];
            let connection_lines = vec![[0f32; 4]; MAX_BLOBS];
            let tracked = vec![TrackedBlob::default(); MAX_BLOBS];

            OwnerState {
                downsample_rt,
                readback: ReadbackRequest::new(),
                readback_w: rw,
                readback_h: rh,
                has_blob_data: false,
                _pixel_buffer: pixel_buffer,
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
    //
    // The worker is shared across owners. To prevent drain-to-latest from
    // discarding one owner's request when another submits, Phase 2 only
    // submits if the worker is idle. Each owner gets served round-robin
    // across frames.
    fn poll_readback(&mut self, owner_key: i64) {
        // ── Phase 1: check if the background worker has a result ──
        if let Some(worker) = &mut self.worker
            && let Some(response) = worker.try_recv()
        {
            // Route result to the owner that submitted it (embedded in response).
            if let Some(state) = self.owner_states.get_mut(&response.owner_key) {
                Self::apply_blob_response(state, &response);
            }
        }

        // ── Phase 2: check for new pixel data from GPU readback ──
        let Some(worker) = &self.worker else {
            return;
        };
        // Only submit if worker is idle — prevents overwriting another owner's
        // in-flight request via drain-to-latest semantics.
        if worker.is_busy() {
            return;
        }

        let Some(state) = self.owner_states.get_mut(&owner_key) else {
            return;
        };

        let pixels = match state.readback.try_read() {
            Some(p) => p,
            None => return,
        };

        // BlobTrackingFX.cs line 195: if (nativeHandle == IntPtr.Zero) return;
        let Some(worker) = &mut self.worker else {
            return;
        };

        // Submit to background worker (non-blocking).
        worker.submit(BlobRequest {
            owner_key,
            pixel_buffer: pixels,
            width: state.readback_w as i32,
            height: state.readback_h as i32,
            threshold: state.pending_threshold,
            sensitivity: state.pending_sensitivity,
        });
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
            let dx = state.native_blob_output[d * 4];
            // The C++ plugin outputs Y in Unity UV convention (v=0 at bottom).
            // Keep as-is: the overlay shader uses a Y-flipped draw_uv that matches
            // Unity's convention, so blob positions flow through unchanged.
            let dy = state.native_blob_output[d * 4 + 1];
            let dw = state.native_blob_output[d * 4 + 2];
            let dh = state.native_blob_output[d * 4 + 3];

            let mut best_dist_sq = MATCH_RADIUS_SQ;
            let mut best_idx: i32 = -1;

            for t in 0..state.tracked_count {
                if state.tracked[t].matched {
                    continue;
                }
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
fn create_font_atlas(device: &GpuDevice) -> GpuTexture {
    const GW: usize = 5;
    const GH: usize = 7;
    const COLS: usize = 16;
    const ROWS: usize = 2;
    let tex_w = COLS * GW;
    let tex_h = ROWS * GH;

    // BlobTrackingFX.cs lines 391-414 — glyph bitmaps (IDENTICAL to Unity source)
    let glyphs: &[&[&str]] = &[
        &[
            ".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###.",
        ], // 0
        &[
            "..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###.",
        ], // 1
        &[
            ".###.", "#...#", "....#", "..##.", ".#...", "#....", "#####",
        ], // 2
        &[
            ".###.", "#...#", "....#", "..##.", "....#", "#...#", ".###.",
        ], // 3
        &[
            "...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#.",
        ], // 4
        &[
            "#####", "#....", "####.", "....#", "....#", "#...#", ".###.",
        ], // 5
        &[
            ".###.", "#....", "#....", "####.", "#...#", "#...#", ".###.",
        ], // 6
        &[
            "#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#...",
        ], // 7
        &[
            ".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###.",
        ], // 8
        &[
            ".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##..",
        ], // 9
        &[
            ".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
        ], // A
        &[
            "####.", "#...#", "#...#", "####.", "#...#", "#...#", "####.",
        ], // B
        &[
            ".###.", "#...#", "#....", "#....", "#....", "#...#", ".###.",
        ], // C
        &[
            "####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####.",
        ], // D
        &[
            "#####", "#....", "#....", "####.", "#....", "#....", "#####",
        ], // E
        &[
            "#####", "#....", "#....", "####.", "#....", "#....", "#....",
        ], // F
        &[
            "#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#",
        ], // X
        &[
            "#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#..",
        ], // Y
        &[
            ".....", ".....", ".....", ".....", ".....", ".....", "..#..",
        ], // .
        &[
            ".....", "..#..", "..#..", ".....", "..#..", "..#..", ".....",
        ], // :
        &[
            "##..#", "##.#.", "..#..", "..#..", "..#..", ".#.##", "#..##",
        ], // %
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
    // RGBA32 → Rgba8Unorm. SHADER_READ + COPY_DST for upload.
    let texture = device.create_texture(&GpuTextureDesc {
        width: tex_w as u32,
        height: tex_h as u32,
        depth: 1,
        format: GpuTextureFormat::Rgba8Unorm,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::COPY_DST
            | GpuTextureUsage::CPU_UPLOAD,
        label: "BlobTracking FontAtlas",
    });

    // Upload pixel data via GpuDevice::upload_texture (Metal replace_region).
    let flat: Vec<u8> = pixels.iter().flat_map(|p| p.iter().copied()).collect();
    device.upload_texture(&texture, &flat);

    texture
}

// Compute variant of the downsample blit — bilinear sample, write to storage texture.
// Eliminates TBDR tile overhead for the downsample pass.
const DOWNSAMPLE_COMPUTE_SHADER: &str = r#"
@group(0) @binding(0) var source_tex: texture_2d<f32>;
@group(0) @binding(1) var tex_sampler: sampler;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(16, 16)
fn cs_downsample(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let color = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    textureStore(output_tex, vec2<i32>(id.xy), color);
}
"#;

impl PostProcessEffect for BlobTrackingFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::BLOB_TRACKING
    }

    // BlobTrackingFX.cs line 127: if (amount <= 0f || material == null) return;
    fn should_skip(&self, fx: &EffectInstance) -> bool {
        fx.param_values.first().copied().unwrap_or(0.0) <= 0.0
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &GpuTexture,
        target: &GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        // BlobTrackingFX.cs line 126-127
        let amount = fx.param_values.first().copied().unwrap_or(0.0);
        if amount <= 0.0 {
            return;
        }

        // BlobTrackingFX.cs lines 129-131
        let threshold = fx.param_values.get(1).copied().unwrap_or(0.65);
        let sensitivity = fx.param_values.get(2).copied().unwrap_or(0.85);
        let smoothing = fx.param_values.get(3).copied().unwrap_or(0.7);
        let connect_dist = fx.param_values.get(4).copied().unwrap_or(0.35);

        // BlobTrackingFX.cs line 133
        let source_w = source.width;
        let source_h = source.height;
        self.get_or_create_owner(gpu.device, gpu.pool, ctx.owner_key, source_w, source_h);

        // ---- Phase 0: poll any pending readback from a previous frame ----
        self.poll_readback(ctx.owner_key);

        let state = self.owner_states.get_mut(&ctx.owner_key).unwrap();
        let rb_w = state.readback_w;
        let rb_h = state.readback_h;

        // ---- Phase 1: Blit to downsample RT and request readback (throttled) ----
        let frame = ctx.frame_count;
        // Guard: if frame_count jumped backwards (export restart, seek), reset
        // so the throttle doesn't stall for hundreds of frames.
        if frame < state.last_readback_frame {
            state.last_readback_frame = frame - READBACK_INTERVAL_FRAMES;
        }
        if !state.readback.is_pending()
            && frame - state.last_readback_frame >= READBACK_INTERVAL_FRAMES
        {
            // Compute downsample dispatch
            gpu.native_enc.dispatch_compute(
                &self.compute_downsample,
                &[
                    GpuBinding::Texture {
                        binding: 0,
                        texture: source,
                    },
                    GpuBinding::Sampler {
                        binding: 1,
                        sampler: &self.sampler,
                    },
                    GpuBinding::Texture {
                        binding: 2,
                        texture: &state.downsample_rt.texture,
                    },
                ],
                [rb_w.div_ceil(16), rb_h.div_ceil(16), 1],
                "BlobTracking Downsample",
            );

            // Readback via native copy_texture_to_buffer
            state.readback.submit(
                gpu,
                &state.downsample_rt.texture,
                rb_w,
                rb_h,
            );
            state.pending_threshold = threshold;
            state.pending_sensitivity = sensitivity;
            state.last_readback_frame = frame;
        }

        // ---- Phase 2: Per-frame temporal smoothing ----
        if state.has_blob_data {
            Self::update_smoothing(state, smoothing, ctx.dt);
        }

        // ---- Phase 3: Render overlay with smoothed blob data ----
        for i in 0..state.tracked_count {
            let t = state.tracked[i];
            state.blob_data_for_shader[i] = [
                t.smooth_pos[0],
                t.smooth_pos[1],
                t.smooth_size[0],
                t.smooth_size[1],
            ];
        }
        for i in state.tracked_count..MAX_BLOBS {
            state.blob_data_for_shader[i] = [0.0; 4];
        }

        state.blob_count = state.tracked_count as i32;
        Self::compute_connections(state, connect_dist);

        let mut blob_center_size = [[0f32; 4]; MAX_BLOBS];
        let mut blob_connections_arr = [[0f32; 4]; MAX_BLOBS];
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

        // Overlay compute dispatch with inline uniform bytes
        let uniform_bytes = bytemuck::bytes_of(&uniforms);
        gpu.native_enc.dispatch_compute(
            &self.compute_overlay,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: uniform_bytes,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: source,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: &self.font_atlas,
                },
                GpuBinding::Sampler {
                    binding: 4,
                    sampler: &self.point_sampler,
                },
                GpuBinding::Texture {
                    binding: 5,
                    texture: target,
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "BlobTracking Overlay",
        );
    }

    // BlobTrackingFX.cs lines 329-333 — ClearState() (all owners)
    fn clear_state(&mut self) {
        for state in self.owner_states.values_mut() {
            clear_owner_state(state);
        }
    }

    fn flush_background_work(&mut self) {
        let Some(worker) = &mut self.worker else { return; };
        if let Some(response) = worker.recv_blocking()
            && let Some(state) = self.owner_states.get_mut(&response.owner_key)
        {
            Self::apply_blob_response(state, &response);
        }
    }

    fn resize(&mut self, _device: &GpuDevice, _width: u32, _height: u32) {
        // BlobTrackingFX.cs lines 366-368:
        // "Downsample RT is fixed size, no resize needed"
    }

    fn cleanup_owner_state(&mut self, owner_key: i64) {
        self.owner_states.remove(&owner_key);
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
    fn cleanup_all_owners(&mut self, _device: &GpuDevice) {
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
