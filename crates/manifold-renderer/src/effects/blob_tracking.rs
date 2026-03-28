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
const MAX_BLOBS: usize = 16;
const READBACK_INTERVAL_FRAMES: i64 = 3;

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

// Tracking constants.
const MATCH_RADIUS_SQ: f32 = 0.08;
const UNASSIGNED_COST: f32 = 1e6;
const MAX_UNSEEN_FRAMES: u32 = 10; // ~0.5s at readback every 3 frames

// 1D Kalman filter for position+velocity tracking.
// State: [position, velocity]. Constant-velocity motion model.
#[derive(Clone, Copy)]
struct KalmanFilter1D {
    x: f32,   // position estimate
    v: f32,   // velocity estimate
    p00: f32, // covariance: position variance
    p01: f32, // covariance: position-velocity
    p10: f32, // covariance: velocity-position
    p11: f32, // covariance: velocity variance
}

impl Default for KalmanFilter1D {
    fn default() -> Self {
        Self { x: 0.0, v: 0.0, p00: 0.0, p01: 0.0, p10: 0.0, p11: 0.0 }
    }
}

impl KalmanFilter1D {
    fn new(pos: f32) -> Self {
        Self {
            x: pos,
            v: 0.0,
            p00: 0.001, // known initial position
            p01: 0.0,
            p10: 0.0,
            p11: 1.0, // unknown initial velocity
        }
    }

    /// Predict step: advance state by dt using constant-velocity model.
    fn predict(&mut self, dt: f32, q_pos: f32, q_vel: f32) {
        self.x += self.v * dt;
        // P_pred = F * P * F' + Q, where F = [[1, dt], [0, 1]]
        let fp00 = self.p00 + dt * self.p10;
        let fp01 = self.p01 + dt * self.p11;
        self.p00 = fp00 + dt * fp01 + q_pos * dt;
        self.p01 = fp01;
        self.p10 += dt * self.p11;
        self.p11 += q_vel * dt;
    }

    /// Update step: correct state with a measurement.
    fn update(&mut self, measurement: f32, r: f32) {
        let innovation = measurement - self.x;
        let s = self.p00 + r;
        if s.abs() < 1e-12 {
            return;
        }
        let s_inv = 1.0 / s;
        let k0 = self.p00 * s_inv;
        let k1 = self.p10 * s_inv;
        self.x += k0 * innovation;
        self.v += k1 * innovation;
        // P = (I - K*H) * P
        let p00 = (1.0 - k0) * self.p00;
        let p01 = (1.0 - k0) * self.p01;
        let p10 = self.p10 - k1 * self.p00;
        let p11 = self.p11 - k1 * self.p01;
        self.p00 = p00;
        self.p01 = p01;
        self.p10 = p10;
        self.p11 = p11;
    }
}

/// Tracked blob with Kalman-filtered position and size.
#[derive(Clone, Copy, Default)]
struct KalmanBlob {
    kf_x: KalmanFilter1D,
    kf_y: KalmanFilter1D,
    kf_w: KalmanFilter1D,
    kf_h: KalmanFilter1D,
    frames_since_seen: u32,
    _id: u32,
}

impl KalmanBlob {
    fn new(x: f32, y: f32, w: f32, h: f32, id: u32) -> Self {
        Self {
            kf_x: KalmanFilter1D::new(x),
            kf_y: KalmanFilter1D::new(y),
            kf_w: KalmanFilter1D::new(w),
            kf_h: KalmanFilter1D::new(h),
            frames_since_seen: 0,
            _id: id,
        }
    }
}

struct OwnerState {
    downsample_rt: RenderTarget,
    readback: ReadbackRequest,
    readback_w: u32,
    readback_h: u32,
    has_blob_data: bool,
    _pixel_buffer: Vec<u8>,
    native_blob_output: Vec<f32>, // MAX_BLOBS * 4: [x, y, w, h] per blob
    blob_data_for_shader: Vec<[f32; 4]>,
    connection_lines: Vec<[f32; 4]>,
    blob_count: i32,
    connection_count: i32,
    pending_threshold: f32,
    pending_sensitivity: f32,
    last_readback_frame: i64,
    // Kalman-filtered tracks
    tracks: [KalmanBlob; MAX_BLOBS],
    track_count: usize,
    next_blob_id: u32,
    detection_count: usize,
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
    blob_center_size: [[f32; 4]; 16],
    // _BlobConnections[MAX_BLOBS] — each vec4 is [ax, ay, bx, by]
    blob_connections: [[f32; 4]; 16],
}

const _: () = assert!(std::mem::size_of::<BlobUniforms>() == 544);

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
                tracks: [KalmanBlob::default(); MAX_BLOBS],
                track_count: 0,
                next_blob_id: 0,
                detection_count: 0,
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

    // Store detection results for processing in update_tracking.
    fn apply_blob_response(state: &mut OwnerState, response: &BlobResponse) {
        let copy_len = response.blob_data.len().min(state.native_blob_output.len());
        state.native_blob_output[..copy_len]
            .copy_from_slice(&response.blob_data[..copy_len]);
        state.detection_count = response.blob_count as usize;
        state.has_new_detection = true;
        state.has_blob_data = true;
    }

    // Kalman predict all tracks, then process new detections if available.
    fn update_tracking(state: &mut OwnerState, smoothing: f32, dt: f32) {
        let (q_pos, q_vel, r) = kalman_params(smoothing);
        // Predict step: advance all tracks by dt
        for i in 0..state.track_count {
            state.tracks[i].kf_x.predict(dt, q_pos, q_vel);
            state.tracks[i].kf_y.predict(dt, q_pos, q_vel);
            state.tracks[i].kf_w.predict(dt, q_pos, q_vel);
            state.tracks[i].kf_h.predict(dt, q_pos, q_vel);
        }
        // Update step: process new detections via Hungarian assignment
        if state.has_new_detection {
            Self::process_detections(state, r);
            state.has_new_detection = false;
        }
    }

    // Hungarian assignment + Kalman update for new detections.
    #[allow(clippy::needless_range_loop)]
    fn process_detections(state: &mut OwnerState, r: f32) {
        let n_det = state.detection_count;
        let n_track = state.track_count;

        if n_det == 0 {
            for i in 0..n_track {
                state.tracks[i].frames_since_seen += 1;
            }
            remove_stale_tracks(state);
            return;
        }

        if n_track == 0 {
            for d in 0..n_det.min(MAX_BLOBS) {
                let (dx, dy, dw, dh) = detection_at(state, d);
                state.tracks[state.track_count] =
                    KalmanBlob::new(dx, dy, dw, dh, state.next_blob_id);
                state.track_count += 1;
                state.next_blob_id = state.next_blob_id.wrapping_add(1);
            }
            return;
        }

        // Build cost matrix (detection × track), gated by MATCH_RADIUS_SQ
        let n = n_det.max(n_track);
        let mut cost = [[UNASSIGNED_COST; MAX_BLOBS]; MAX_BLOBS];
        for d in 0..n_det {
            let (dx, dy, _, _) = detection_at(state, d);
            for t in 0..n_track {
                let ex = state.tracks[t].kf_x.x - dx;
                let ey = state.tracks[t].kf_y.x - dy;
                let dist_sq = ex * ex + ey * ey;
                if dist_sq < MATCH_RADIUS_SQ {
                    cost[d][t] = dist_sq;
                }
            }
        }

        let assignment = hungarian_solve(&cost, n);

        let mut track_matched = [false; MAX_BLOBS];

        for d in 0..n_det {
            let t = assignment[d];
            if t < n_track && cost[d][t] < MATCH_RADIUS_SQ {
                // Matched — Kalman update with measurement
                let (dx, dy, dw, dh) = detection_at(state, d);
                state.tracks[t].kf_x.update(dx, r);
                state.tracks[t].kf_y.update(dy, r);
                state.tracks[t].kf_w.update(dw, r);
                state.tracks[t].kf_h.update(dh, r);
                state.tracks[t].frames_since_seen = 0;
                track_matched[t] = true;
            } else if state.track_count < MAX_BLOBS {
                // Unmatched detection — birth new track
                let (dx, dy, dw, dh) = detection_at(state, d);
                state.tracks[state.track_count] =
                    KalmanBlob::new(dx, dy, dw, dh, state.next_blob_id);
                state.track_count += 1;
                state.next_blob_id = state.next_blob_id.wrapping_add(1);
            }
        }

        // Age unmatched tracks
        for t in 0..n_track {
            if !track_matched[t] {
                state.tracks[t].frames_since_seen += 1;
            }
        }

        remove_stale_tracks(state);
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

/// Map smoothing parameter (0=responsive, 1=smooth) to Kalman noise parameters.
/// Returns (q_pos, q_vel, r) — process noise and measurement noise.
fn kalman_params(smoothing: f32) -> (f32, f32, f32) {
    let s = smoothing.clamp(0.0, 1.0);
    // Process noise: how much state can change unexpectedly per frame
    let q_pos = 0.0001 + (1.0 - s) * 0.005; // position jitter
    let q_vel = 0.001 + (1.0 - s) * 0.05; // velocity change
    // Measurement noise: how much we trust detections
    let r = 0.0005 + s * 0.05;
    (q_pos, q_vel, r)
}

/// Read detection d from native_blob_output buffer.
fn detection_at(state: &OwnerState, d: usize) -> (f32, f32, f32, f32) {
    (
        state.native_blob_output[d * 4],
        state.native_blob_output[d * 4 + 1],
        state.native_blob_output[d * 4 + 2],
        state.native_blob_output[d * 4 + 3],
    )
}

/// Remove tracks not seen for MAX_UNSEEN_FRAMES detection cycles.
fn remove_stale_tracks(state: &mut OwnerState) {
    let mut write = 0;
    for read in 0..state.track_count {
        if state.tracks[read].frames_since_seen <= MAX_UNSEEN_FRAMES {
            if write != read {
                state.tracks[write] = state.tracks[read];
            }
            write += 1;
        }
    }
    state.track_count = write;
}

/// Hungarian algorithm (Kuhn-Munkres) for minimum-cost assignment.
/// Solves the n×n assignment problem on the first n rows/cols of cost.
/// Returns row_to_col mapping; MAX_BLOBS means unassigned.
fn hungarian_solve(
    cost: &[[f32; MAX_BLOBS]; MAX_BLOBS],
    n: usize,
) -> [usize; MAX_BLOBS] {
    let mut result = [MAX_BLOBS; MAX_BLOBS];
    if n == 0 {
        return result;
    }

    // Use f64 internally to avoid precision issues with UNASSIGNED_COST (1e6).
    let mut u = [0.0f64; MAX_BLOBS + 1]; // row potentials (1-indexed)
    let mut v = [0.0f64; MAX_BLOBS + 1]; // col potentials (1-indexed)
    let mut p = [0usize; MAX_BLOBS + 1]; // p[j] = row assigned to col j
    let mut way = [0usize; MAX_BLOBS + 1];

    for i in 1..=n {
        let mut min_v = [f64::MAX; MAX_BLOBS + 1];
        let mut used = [false; MAX_BLOBS + 1];
        p[0] = i;
        let mut j0 = 0usize;

        loop {
            used[j0] = true;
            let i0 = p[j0];
            let mut delta = f64::MAX;
            let mut j1 = 0usize;

            for j in 1..=n {
                if !used[j] {
                    let cur =
                        cost[i0 - 1][j - 1] as f64 - u[i0] - v[j];
                    if cur < min_v[j] {
                        min_v[j] = cur;
                        way[j] = j0;
                    }
                    if min_v[j] < delta {
                        delta = min_v[j];
                        j1 = j;
                    }
                }
            }

            for j in 0..=n {
                if used[j] {
                    u[p[j]] += delta;
                    v[j] -= delta;
                } else {
                    min_v[j] -= delta;
                }
            }

            j0 = j1;
            if p[j0] == 0 {
                break;
            }
        }

        loop {
            let j1 = way[j0];
            p[j0] = p[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }

    // Convert column→row assignment to row→column
    for j in 1..=n {
        if p[j] > 0 && p[j] <= n {
            result[p[j] - 1] = j - 1;
        }
    }
    result
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
            Self::update_tracking(state, smoothing, ctx.dt);
        }

        // ---- Phase 3: Render overlay with smoothed blob data ----
        for i in 0..state.track_count {
            let t = &state.tracks[i];
            state.blob_data_for_shader[i] = [
                t.kf_x.x,
                t.kf_y.x,
                t.kf_w.x.max(0.0),
                t.kf_h.x.max(0.0),
            ];
        }
        for i in state.track_count..MAX_BLOBS {
            state.blob_data_for_shader[i] = [0.0; 4];
        }

        state.blob_count = state.track_count as i32;
        Self::compute_connections(state, connect_dist);

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
    state.track_count = 0;
    state.detection_count = 0;
    state.has_new_detection = false;
}
