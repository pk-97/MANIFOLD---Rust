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
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::ParamSpec;
use manifold_core::effects::EffectInstance;
use crate::effects::registration::EffectFactory;
use manifold_gpu::{
    GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuBuffer, GpuComputePipeline,
    GpuDevice, GpuFilterMode, GpuLoadAction, GpuRenderPipeline, GpuSampler, GpuSamplerDesc,
    GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::BLOB_TRACKING,
        display_name: "Blob Tracking",
        category: "Post-Process",
        available: true,
        osc_prefix: "blobTracking",
        legacy_discriminant: Some(22),
        params: &[
            ParamSpec::continuous("Amount", 0.0, 1.0, 0.0, "F2", ""),
            ParamSpec::continuous("Thresh", 0.05, 0.9, 0.65, "F2", "Threshold"),
            ParamSpec::continuous("Sens", 0.2, 1.0, 0.85, "F2", "Sensitivity"),
            ParamSpec::continuous("Smooth", 0.0, 1.0, 0.7, "F2", "Smoothing"),
            ParamSpec::continuous("Connect", 0.0, 1.0, 0.35, "F2", "Connect"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::BLOB_TRACKING,
        create: |device| Box::new(BlobTrackingFX::new(device)),
    }
}

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
const READBACK_INTERVAL_FRAMES: i64 = 3;

/// One-Euro filter smoothing factor from cutoff frequency (Hz) and timestep.
/// α = 1 / (1 + τ/dt), where τ = 1/(2π·fc).
#[inline]
fn one_euro_alpha(dt: f32, cutoff: f32) -> f32 {
    let tau = 1.0 / (2.0 * std::f32::consts::PI * cutoff);
    1.0 / (1.0 + tau / dt)
}

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
// Grace period: unmatched blobs survive this many detection cycles before removal.
const UNMATCHED_GRACE_FRAMES: u32 = 3;

// One-Euro filter parameters.
// min_cutoff: minimum cutoff frequency (Hz). Lower = more smoothing when still.
//   Controlled by the "Smooth" param: smooth=0 → 4.0 Hz (responsive), smooth=1 → 0.3 Hz (stable).
// beta: speed coefficient. Higher = more responsiveness during fast motion.
const ONE_EURO_BETA: f32 = 0.5;
// Cutoff frequency for the derivative low-pass filter (Hz). Fixed.
const ONE_EURO_D_CUTOFF: f32 = 1.0;

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
    /// How many consecutive detection cycles this blob went unmatched.
    missed_count: u32,
    // One-Euro filter state: filtered derivative for position and size (per axis).
    dx_pos: [f32; 2],
    dx_size: [f32; 2],
}

// BlobTrackingFX.cs line 48-68 — OwnerState
struct OwnerState {
    downsample_rt: RenderTarget,
    readback: ReadbackRequest,
    readback_w: u32,
    readback_h: u32,
    has_blob_data: bool,
    _pixel_buffer: Vec<u8>,
    native_blob_output: Vec<f32>,        // new float[MAX_BLOBS * 4]
    blob_data_for_shader: Vec<[f32; 4]>, // Vector4[MAX_BLOBS]
    connection_lines: Vec<[f32; 4]>,     // Vector4[MAX_BLOBS]
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

// ---- Geometry-based overlay types ----

const MAX_OVERLAY_QUADS: usize = 512;

/// Per-instance data for overlay quad rendering. Matches WGSL QuadInstance.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OverlayQuad {
    rect: [f32; 4],       // clip-space (x0, y0, x1, y1)
    atlas_rect: [f32; 4], // font atlas UVs (u0, v0, u1, v1); (0,0,0,0) for solid
    alpha: f32,
    _pad: [f32; 3],
}

const _: () = assert!(std::mem::size_of::<OverlayQuad>() == 48);

/// Uniforms for the instanced overlay render shader.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OverlayUniforms {
    overlay_color: [f32; 3],
    amount: f32,
}

// BlobTrackingFX.cs line 10 — BlobTrackingFX : IPostProcessEffect, IStatefulEffect
pub struct BlobTrackingFX {
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

    // ---- Geometry-based overlay rendering ----
    overlay_pipeline: GpuRenderPipeline,
    overlay_buf: GpuBuffer,
    overlay_quads: Vec<OverlayQuad>,
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
        let compute_downsample = device.create_compute_pipeline(
            DOWNSAMPLE_COMPUTE_SHADER,
            "cs_downsample",
            "BlobTracking Downsample",
        );

        // ---- Geometry overlay pipeline ----
        // Additive blend: overlay color is added to the existing target contents.
        // Fragment outputs premultiplied RGB (color * alpha * amount), alpha = 0
        // so destination alpha is preserved.
        let blend = GpuBlendState {
            src_factor: GpuBlendFactor::One,
            dst_factor: GpuBlendFactor::One,
            operation: GpuBlendOp::Add,
            src_alpha_factor: GpuBlendFactor::Zero,
            dst_alpha_factor: GpuBlendFactor::One,
            alpha_operation: GpuBlendOp::Add,
        };
        let overlay_pipeline = device.create_render_pipeline(
            include_str!("shaders/fx_blob_overlay_render.wgsl"),
            "vs_main",
            "fs_main",
            GpuTextureFormat::Rgba16Float,
            Some(blend),
            "BlobTracking OverlayRender",
        );
        let overlay_buf = device
            .create_buffer_shared((MAX_OVERLAY_QUADS * std::mem::size_of::<OverlayQuad>()) as u64);

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
            compute_downsample,
            sampler,
            point_sampler,
            font_atlas,
            worker,
            owner_states: AHashMap::new(),
            overlay_pipeline,
            overlay_buf,
            overlay_quads: Vec::with_capacity(MAX_OVERLAY_QUADS),
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

        // Mark all existing tracked blobs as unmatched for this cycle
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
                state.tracked[idx].missed_count = 0;
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
                    missed_count: 0,
                    dx_pos: [0.0; 2],
                    dx_size: [0.0; 2],
                };
            }
        }

        // Increment missed_count for blobs that weren't matched this cycle
        for i in 0..state.tracked_count {
            if !state.tracked[i].matched {
                state.tracked[i].missed_count += 1;
            }
        }

        state.has_new_detection = true;
        state.has_blob_data = true;
    }

    // One-Euro filter smoothing — replaces the original single-pole exponential.
    // Adapts cutoff frequency based on speed: stable when slow, responsive when fast.
    // Reference: Casiez et al., "1€ Filter", CHI 2012.
    fn update_smoothing(state: &mut OwnerState, smoothing: f32, dt: f32) {
        // Remove blobs that have exceeded the grace period (missed too many
        // consecutive detection cycles). Blobs within the grace window are kept
        // and continue smoothing toward their last known position.
        if state.has_new_detection {
            let mut write = 0usize;
            for read in 0..state.tracked_count {
                if state.tracked[read].missed_count <= UNMATCHED_GRACE_FRAMES {
                    if write != read {
                        state.tracked[write] = state.tracked[read];
                    }
                    write += 1;
                }
            }
            state.tracked_count = write;
            state.has_new_detection = false;
        }

        if dt <= 0.0 {
            return;
        }

        // Map "Smooth" param (0–1) to min_cutoff:
        //   smooth=0 → 4.0 Hz (very responsive, minimal filtering)
        //   smooth=1 → 0.3 Hz (heavy filtering when still)
        let s = smoothing.clamp(0.0, 1.0);
        let min_cutoff = 4.0 + (0.3 - 4.0) * s;

        // Derivative filter alpha (fixed cutoff)
        let d_alpha = one_euro_alpha(dt, ONE_EURO_D_CUTOFF);

        for i in 0..state.tracked_count {
            let b = &mut state.tracked[i];

            // --- Position (2 axes) ---
            for ax in 0..2 {
                let raw_dx = (b.raw_pos[ax] - b.smooth_pos[ax]) / dt;
                // Low-pass the derivative
                b.dx_pos[ax] = b.dx_pos[ax] + d_alpha * (raw_dx - b.dx_pos[ax]);
                // Adaptive cutoff: faster motion → higher cutoff → less smoothing
                let cutoff = min_cutoff + ONE_EURO_BETA * b.dx_pos[ax].abs();
                let alpha = one_euro_alpha(dt, cutoff);
                b.smooth_pos[ax] = b.smooth_pos[ax] + alpha * (b.raw_pos[ax] - b.smooth_pos[ax]);
            }

            // --- Size (2 axes) ---
            for ax in 0..2 {
                let raw_dx = (b.raw_size[ax] - b.smooth_size[ax]) / dt;
                b.dx_size[ax] = b.dx_size[ax] + d_alpha * (raw_dx - b.dx_size[ax]);
                let cutoff = min_cutoff + ONE_EURO_BETA * b.dx_size[ax].abs();
                let alpha = one_euro_alpha(dt, cutoff);
                b.smooth_size[ax] =
                    b.smooth_size[ax] + alpha * (b.raw_size[ax] - b.smooth_size[ax]);
            }
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

    // ---- Geometry overlay quad generation ----
    // Translates each procedural drawing operation from the compute shader into
    // CPU-side OverlayQuad instances. All coordinates start in "draw_uv" space
    // [0,1] (Y-up, Unity convention), then convert to clip space for the vertex
    // shader: clip = draw_uv * 2.0 - 1.0.

    fn generate_overlay_quads(
        quads: &mut Vec<OverlayQuad>,
        state: &OwnerState,
        width: u32,
        height: u32,
    ) {
        let dpi_scale = height as f32 / 1080.0;
        let px_u = (1.0 / width as f32) * dpi_scale;
        let px_v = (1.0 / height as f32) * dpi_scale;
        // Per-axis thicknesses so overlays are aspect-ratio independent.
        let thick_u = 2.0 * px_u;
        let thick_v = 2.0 * px_v;
        let thin_u = 1.5 * px_u;
        let thin_v = 1.5 * px_v;
        let digit_w = px_u * 2.0;
        let digit_h = px_v * 2.0;

        // Per-blob overlays
        for b in 0..state.blob_count as usize {
            if quads.len() >= MAX_OVERLAY_QUADS {
                break;
            }
            let blob = state.blob_data_for_shader[b];
            let center = [blob[0], blob[1]];
            let half_size = [blob[2] * 0.5, blob[3] * 0.5];

            let bracket_len = half_size[0].min(half_size[1]) * 0.4;

            // (a) Corner brackets — 4 corners × 2 arms = 8 quads
            for &dir in &[[-1.0f32, -1.0f32], [1.0, -1.0], [-1.0, 1.0], [1.0, 1.0]] {
                let corner = [
                    center[0] + half_size[0] * dir[0],
                    center[1] + half_size[1] * dir[1],
                ];
                // Horizontal arm (spans X, thickness in Y)
                let (hx0, hx1) = if dir[0] < 0.0 {
                    (corner[0], corner[0] + bracket_len)
                } else {
                    (corner[0] - bracket_len, corner[0])
                };
                push_solid(
                    quads,
                    hx0,
                    corner[1] - thick_v / 2.0,
                    hx1,
                    corner[1] + thick_v / 2.0,
                    1.0,
                );
                // Vertical arm (spans Y, thickness in X)
                let (vy0, vy1) = if dir[1] < 0.0 {
                    (corner[1], corner[1] + bracket_len)
                } else {
                    (corner[1] - bracket_len, corner[1])
                };
                push_solid(
                    quads,
                    corner[0] - thick_u / 2.0,
                    vy0,
                    corner[0] + thick_u / 2.0,
                    vy1,
                    1.0,
                );
            }

            // (b) Crosshair — 2 quads
            let ch_size = half_size[0].min(half_size[1]) * 0.3;
            // Horizontal crosshair (spans X, thickness in Y)
            push_solid(
                quads,
                center[0] - ch_size,
                center[1] - thin_v / 2.0,
                center[0] + ch_size,
                center[1] + thin_v / 2.0,
                1.0,
            );
            // Vertical crosshair (spans Y, thickness in X)
            push_solid(
                quads,
                center[0] - thin_u / 2.0,
                center[1] - ch_size,
                center[0] + thin_u / 2.0,
                center[1] + ch_size,
                1.0,
            );

            // (c) Center dot — 1 quad (per-axis radii for visual circle)
            let dot_ru = px_u * 4.0;
            let dot_rv = px_v * 4.0;
            push_solid(
                quads,
                center[0] - dot_ru,
                center[1] - dot_rv,
                center[0] + dot_ru,
                center[1] + dot_rv,
                1.0,
            );

            // (d) Hex label — 4 textured quads
            let hex_pos = [
                center[0] - half_size[0],
                center[1] + half_size[1] + 8.0 * px_v,
            ];
            let hex_id = b as f32 * 17.0 + 48.0;
            let n = hex_id.floor().clamp(0.0, 255.0);
            let hi = (n / 16.0).floor();
            let lo = n % 16.0;
            // Chars: '0'(code 0), 'X'(code 16), hi digit, lo digit
            // at pixel offsets 0, 6, 13, 19
            let glyph_w = 5.0 * digit_w;
            let glyph_h = 7.0 * digit_h;
            for &(char_code, px_offset) in &[(0.0f32, 0.0f32), (16.0, 6.0), (hi, 13.0), (lo, 19.0)]
            {
                let gx = hex_pos[0] + px_offset * digit_w;
                let gy = hex_pos[1];
                let atlas = glyph_atlas_rect(char_code);
                push_textured(quads, gx, gy, gx + glyph_w, gy + glyph_h, atlas, 1.0);
            }

            // (e) Coord label — 7 textured quads
            let coord_pos = [
                center[0] - half_size[0],
                center[1] - half_size[1] - 38.0 * px_v,
            ];
            let x_val = (center[0] * 999.0).floor().clamp(0.0, 999.0);
            let y_val = (center[1] * 999.0).floor().clamp(0.0, 999.0);
            let x_h = (x_val / 100.0).floor();
            let x_t = ((x_val % 100.0) / 10.0).floor();
            let x_o = x_val % 10.0;
            let y_h = (y_val / 100.0).floor();
            let y_t = ((y_val % 100.0) / 10.0).floor();
            let y_o = y_val % 10.0;
            // Pixel offsets: 0, 6, 12, 17(separator), 22, 28, 34
            for &(char_code, px_offset) in &[
                (x_h, 0.0f32),
                (x_t, 6.0),
                (x_o, 12.0),
                (18.0, 17.0), // comma/separator char code 18
                (y_h, 22.0),
                (y_t, 28.0),
                (y_o, 34.0),
            ] {
                let gx = coord_pos[0] + px_offset * digit_w;
                let gy = coord_pos[1];
                let atlas = glyph_atlas_rect(char_code);
                push_textured(quads, gx, gy, gx + glyph_w, gy + glyph_h, atlas, 1.0);
            }

            // (f) Gauge bar — up to 5 quads (4 edges + 1 fill)
            let gauge_pos = [
                center[0] - half_size[0],
                center[1] - half_size[1] - 50.0 * px_v,
            ];
            let gauge_w = (half_size[0] * 2.0).max(80.0 * px_u);
            let gauge_h = 8.0 * px_v;
            let gauge_fill = (blob[2] * blob[3] * 20.0).clamp(0.0, 1.0);

            // Gauge edges: top, bottom, left, right
            let gx0 = gauge_pos[0];
            let gy0 = gauge_pos[1];
            let gx1 = gauge_pos[0] + gauge_w;
            let gy1 = gauge_pos[1] - gauge_h;
            // Top edge (horizontal, thickness in Y)
            push_solid(
                quads,
                gx0,
                gy0 - thin_v / 2.0,
                gx1,
                gy0 + thin_v / 2.0,
                1.0,
            );
            // Bottom edge (horizontal, thickness in Y)
            push_solid(
                quads,
                gx0,
                gy1 - thin_v / 2.0,
                gx1,
                gy1 + thin_v / 2.0,
                1.0,
            );
            // Left edge (vertical, thickness in X)
            push_solid(
                quads,
                gx0 - thin_u / 2.0,
                gy1,
                gx0 + thin_u / 2.0,
                gy0,
                1.0,
            );
            // Right edge (vertical, thickness in X)
            push_solid(
                quads,
                gx1 - thin_u / 2.0,
                gy1,
                gx1 + thin_u / 2.0,
                gy0,
                1.0,
            );
            // Fill quad
            if gauge_fill > 0.0 {
                push_solid(quads, gx0, gy1, gx0 + gauge_w * gauge_fill, gy0, 0.4);
            }

            // (g) Tick marks — 4 quads
            let tick_base = [
                center[0] + half_size[0] + 8.0 * px_u,
                center[1] + half_size[1],
            ];
            let tick_spacing = half_size[1] * 0.5;
            for t in 0..4u32 {
                let tick_y = tick_base[1] - tick_spacing * t as f32;
                let tick_len = if t % 2 == 0 { 12.0 * px_u } else { 6.0 * px_u };
                // Horizontal tick (thickness in Y)
                push_solid(
                    quads,
                    tick_base[0],
                    tick_y - thin_v / 2.0,
                    tick_base[0] + tick_len,
                    tick_y + thin_v / 2.0,
                    0.5,
                );
            }
        }

        // Per-connection overlays
        for c in 0..state.connection_count as usize {
            if quads.len() >= MAX_OVERLAY_QUADS {
                break;
            }
            let conn = state.connection_lines[c];
            let conn_a = [conn[0], conn[1]];
            let conn_b = [conn[2], conn[3]];

            let dx = conn_b[0] - conn_a[0];
            let dy = conn_b[1] - conn_a[1];
            let len = (dx * dx + dy * dy).sqrt();
            if len < 0.001 {
                continue;
            }

            // (h) Dashed connection line
            let dir = [dx / len, dy / len];
            let perp = [-dir[1], dir[0]];
            let dash_total = px_u * 12.0;
            let num_dashes = (len / dash_total).ceil() as u32;
            // Per-axis perpendicular expansion for uniform visual thickness.
            let half_tu = thin_u / 2.0;
            let half_tv = thin_v / 2.0;

            for d in 0..num_dashes {
                if quads.len() >= MAX_OVERLAY_QUADS {
                    break;
                }
                let t0 = d as f32 * dash_total / len;
                if t0 > 1.0 {
                    break;
                }
                let t1 = ((d as f32 * dash_total + dash_total * 0.6) / len).min(1.0);
                // Dash "on" portion: from t0 to t1 along the line
                // The compute shader uses step(0.4, dash_phase) which gives ~60% on
                let p0 = [conn_a[0] + dir[0] * len * t0, conn_a[1] + dir[1] * len * t0];
                let p1 = [conn_a[0] + dir[0] * len * t1, conn_a[1] + dir[1] * len * t1];
                // Expand along perpendicular with per-axis thickness
                let exp_u = perp[0] * half_tu;
                let exp_v = perp[1] * half_tv;
                let min_x = (p0[0] - exp_u)
                    .min(p0[0] + exp_u)
                    .min(p1[0] - exp_u)
                    .min(p1[0] + exp_u);
                let max_x = (p0[0] - exp_u)
                    .max(p0[0] + exp_u)
                    .max(p1[0] - exp_u)
                    .max(p1[0] + exp_u);
                let min_y = (p0[1] - exp_v)
                    .min(p0[1] + exp_v)
                    .min(p1[1] - exp_v)
                    .min(p1[1] + exp_v);
                let max_y = (p0[1] - exp_v)
                    .max(p0[1] + exp_v)
                    .max(p1[1] - exp_v)
                    .max(p1[1] + exp_v);
                push_solid(quads, min_x, min_y, max_x, max_y, 0.5);
            }

            // (i) Midpoint dot — 1 quad (per-axis radii for visual circle)
            let mid = [(conn_a[0] + conn_b[0]) * 0.5, (conn_a[1] + conn_b[1]) * 0.5];
            let mid_ru = px_u * 5.0;
            let mid_rv = px_v * 5.0;
            push_solid(
                quads,
                mid[0] - mid_ru,
                mid[1] - mid_rv,
                mid[0] + mid_ru,
                mid[1] + mid_rv,
                0.4,
            );

            // (j) Distance label — 3 textured quads
            let dist_label_pos = [mid[0] + 8.0 * px_u, mid[1] + 4.0 * px_v];
            let dist_val = (len * 1000.0).floor().clamp(0.0, 999.0);
            let hundreds = (dist_val / 100.0).floor();
            let tens = ((dist_val % 100.0) / 10.0).floor();
            let ones = dist_val % 10.0;
            let small_dw = digit_w * 0.7;
            let small_dh = digit_h * 0.7;
            let small_gw = 5.0 * small_dw;
            let small_gh = 7.0 * small_dh;
            for &(char_code, px_offset) in &[(hundreds, 0.0f32), (tens, 6.0), (ones, 12.0)] {
                let gx = dist_label_pos[0] + px_offset * small_dw;
                let gy = dist_label_pos[1];
                let atlas = glyph_atlas_rect(char_code);
                push_textured(quads, gx, gy, gx + small_gw, gy + small_gh, atlas, 0.6);
            }
        }
    }
}

// ---- Overlay quad helpers ----

/// Convert a draw_uv rect [0,1] to clip-space rect [-1,1].
fn uv_rect_to_clip(x0: f32, y0: f32, x1: f32, y1: f32) -> [f32; 4] {
    [
        x0 * 2.0 - 1.0,
        y0 * 2.0 - 1.0,
        x1 * 2.0 - 1.0,
        y1 * 2.0 - 1.0,
    ]
}

/// Push a solid (untextured) quad in draw_uv space.
fn push_solid(quads: &mut Vec<OverlayQuad>, x0: f32, y0: f32, x1: f32, y1: f32, alpha: f32) {
    if quads.len() >= MAX_OVERLAY_QUADS {
        return;
    }
    quads.push(OverlayQuad {
        rect: uv_rect_to_clip(x0, y0, x1, y1),
        atlas_rect: [0.0; 4],
        alpha,
        _pad: [0.0; 3],
    });
}

/// Push a textured (font glyph) quad in draw_uv space.
fn push_textured(
    quads: &mut Vec<OverlayQuad>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    atlas_rect: [f32; 4],
    alpha: f32,
) {
    if quads.len() >= MAX_OVERLAY_QUADS {
        return;
    }
    quads.push(OverlayQuad {
        rect: uv_rect_to_clip(x0, y0, x1, y1),
        atlas_rect,
        alpha,
        _pad: [0.0; 3],
    });
}

/// Compute font atlas UV rect for a given character code.
/// Atlas is 80×14 pixels, 5×7 glyphs, 16 columns × 2 rows.
fn glyph_atlas_rect(char_code: f32) -> [f32; 4] {
    let c = (char_code + 0.5).floor();
    let atlas_col = (c % 16.0).floor();
    let atlas_row = (c / 16.0).floor();
    let u0 = (atlas_col * 5.0) / 80.0;
    let v0 = (atlas_row * 7.0) / 14.0;
    let u1 = (atlas_col * 5.0 + 5.0) / 80.0;
    let v1 = (atlas_row * 7.0 + 7.0) / 14.0;
    [u0, v0, u1, v1]
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
        mip_levels: 1,
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

        // Phases 1-3 use mutable state borrow; scoped to release before Phase 4.
        {
            let state = self.owner_states.get_mut(&ctx.owner_key).unwrap();
            let rb_w = state.readback_w;
            let rb_h = state.readback_h;

            // ---- Phase 1: Blit to downsample RT and request readback ----
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
                state
                    .readback
                    .submit(gpu, &state.downsample_rt.texture, rb_w, rb_h);
                state.pending_threshold = threshold;
                state.pending_sensitivity = sensitivity;
                state.last_readback_frame = frame;
            }

            // ---- Phase 2: Per-frame temporal smoothing ----
            if state.has_blob_data {
                Self::update_smoothing(state, smoothing, ctx.dt);
            }

            // ---- Phase 3: Prepare smoothed blob data + connections ----
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
        } // mutable borrow of owner_states released

        // ---- Phase 4: Geometry overlay + composite ----
        // Generate overlay quad instances from blob data (shared borrow).
        self.overlay_quads.clear();
        Self::generate_overlay_quads(
            &mut self.overlay_quads,
            self.owner_states.get(&ctx.owner_key).unwrap(),
            ctx.width,
            ctx.height,
        );

        // Copy source → target, then draw overlay directly on top with additive blend.
        // Eliminates the temp overlay texture and full-screen composite pass (~7% savings).
        gpu.native_enc
            .copy_texture_to_texture(source, target, ctx.width, ctx.height, 1);

        let quad_count = self.overlay_quads.len().min(MAX_OVERLAY_QUADS);
        if quad_count > 0 {
            let quad_bytes = bytemuck::cast_slice(&self.overlay_quads[..quad_count]);
            unsafe {
                self.overlay_buf.write(0, quad_bytes);
            }

            let overlay_uniforms = OverlayUniforms {
                overlay_color: [0.85, 0.92, 1.0],
                amount,
            };
            gpu.native_enc.draw_instanced(
                &self.overlay_pipeline,
                target,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&overlay_uniforms),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: &self.overlay_buf,
                        offset: 0,
                    },
                    GpuBinding::Texture {
                        binding: 2,
                        texture: &self.font_atlas,
                    },
                    GpuBinding::Sampler {
                        binding: 3,
                        sampler: &self.point_sampler,
                    },
                ],
                6,
                quad_count as u32,
                GpuLoadAction::Load,
                "BlobTracking Overlay",
            );
        }
    }

    // BlobTrackingFX.cs lines 329-333 — ClearState() (all owners)
    fn clear_state(&mut self) {
        for state in self.owner_states.values_mut() {
            clear_owner_state(state);
        }
    }

    fn flush_background_work(&mut self) {
        let Some(worker) = &mut self.worker else {
            return;
        };
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
