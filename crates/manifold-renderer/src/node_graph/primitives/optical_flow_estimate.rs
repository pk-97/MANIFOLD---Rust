//! `node.optical_flow` — dense optical flow (Farneback)
//! via the MiDaS native plugin's compute_flow path, wrapped as a
//! standalone primitive.
//!
//! Output: Rgba16Float texture with channel layout
//!   R = flow_x (UV units; positive = right)
//!   G = confidence (0..1)
//!   B = flow_y (UV units; positive = down)
//!   A = valid_mask (0 or 1)
//!
//! R/B convention matches `node.flow_field_noise` /
//! `node.uv_displace_by_flow` so this composes directly into any
//! existing displacement pipeline.
//!
//! Frame-to-frame state: the worker holds the previous frame's
//! readback bytes and pairs them with the current frame on each
//! inference. ~2-3 frame latency at default analysis_max_dim=360.
//! `fixed_lag` keeps the same async pipeline but consumes a worker response
//! at the next run deadline for deterministic export/playback convergence.

#![allow(private_interfaces)]

use std::borrow::Cow;

use manifold_foundation::cold_touch::{ColdTouchKind, record_cold_touch};
use manifold_gpu::{
    GpuBinding, GpuComputePipeline, GpuSamplerDesc, GpuTexture, GpuTextureDesc,
    GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use manifold_native::depth_estimator::DepthEstimator;

use crate::background_worker::BackgroundWorker;
use crate::gpu_encoder::GpuEncoder;
use crate::gpu_readback::ReadbackRequest;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

struct FlowRequest {
    /// The CURRENT frame's RGBA8 pixels at analysis resolution.
    /// The worker pairs this against the previous frame it holds in
    /// its own internal state.
    pixel_data: Vec<u8>,
    /// Spare packet returned unchanged after the worker has accepted the
    /// current frame. Keeping this ownership on the packet lets the main
    /// thread reuse a second pixel allocation without cloning the frame.
    spare_pixel_data: Vec<u8>,
    /// Reusable FFI output allocation. The worker returns it in the response
    /// on first-frame and failure paths, and as `flow_packed` on success.
    flow_buffer: Vec<f32>,
    width: i32,
    height: i32,
    generation: u64,
}

struct FlowResponse {
    /// Packed [flow_x, flow_y, confidence, valid_mask] per pixel,
    /// length = width * height * 4. None on first frame (no prev),
    /// model failure, or invalid dims.
    flow_packed: Option<Vec<f32>>,
    /// Returned FFI output when no flow was produced.
    recycled_flow: Vec<f32>,
    /// Previous-frame and packet-spare allocations returned to the content
    /// thread. One is used for the next readback; the other remains a spare.
    recycled_pixel_data: Vec<u8>,
    spare_pixel_data: Vec<u8>,
    width: i32,
    height: i32,
    generation: u64,
    analysis_attempted: bool,
    /// Global-motion-compensated frame-difference score from the
    /// FFI worker. Crosses ~0.28 on hard scene cuts; near zero on
    /// continuous motion. Used downstream to gate state resets in
    /// any stateful primitive (wire cut_score → node.filter →
    /// reset_trigger). Zero when flow_packed is None.
    cut_score: f32,
}

struct FlowState {
    analysis_width: u32,
    analysis_height: u32,
    readback: ReadbackRequest,
    readback_pending: bool,
    has_flow: bool,
    flow_dirty: bool,
    flow_buffer: Vec<f32>,
    /// Reused CPU packet for `ReadbackRequest::try_read_into`.
    pixel_buffer: Vec<u8>,
    /// A second packet is kept when the worker returns both its previous
    /// frame and the spare carried by the request.
    spare_pixel_buffer: Vec<u8>,
    /// Cached packed upload bytes, written directly in native flow-channel
    /// order (x, confidence, y, valid).
    upload_bytes: Vec<u8>,
    flow_texture: GpuTexture,
    /// Analysis-res downscale target for the readback. Cached here (rebuilt only
    /// when analysis dims change) so `run` never allocates per readback cadence.
    staging_texture: GpuTexture,
    last_request_frame: i64,
    frame_counter: i64,
    /// Latest cut_score from the FFI worker. Held here so the
    /// scalar output port can re-emit it every frame, including on
    /// frames when no new inference completed.
    cut_score: f32,
    /// True once the worker has returned its first successful flow inference.
    first_response_delivered: bool,
    /// Warmup also completes on an unavailable plugin or a failed first
    /// analysis, so load-time convergence cannot hang forever.
    warmup_complete: bool,
    /// Prevents repeating the same failed-analysis diagnostic every frame;
    /// a successful analysis arms the next failure transition.
    analysis_failed: bool,
    generation: u64,
    clear_texture_pending: bool,
}

crate::primitive! {
    name: OpticalFlowEstimate,
    type_id: "node.optical_flow",
    purpose: "Dense optical flow (Farneback + global motion compensation) via the MiDaS native plugin. Wraps FfiDepthEstimator::compute_flow on a background worker that holds the previous frame internally and pairs it with the current. Input: any Texture2D. Outputs: (a) Rgba16Float flow map with R=flow_x, G=confidence, B=flow_y, A=valid_mask (R/B layout matches node.flow_field_noise and node.uv_displace_by_flow); (b) scalar cut_score — global-motion-compensated frame-difference, crosses ~0.28 on hard scene cuts, near zero on continuous motion.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        // R = flow_x (UV units; positive = right)
        // G = confidence (0..1)
        // B = flow_y (UV units; positive = down)
        // A = valid_mask (0 or 1)
        //
        // Watercolor convention — R and B carry the flow components,
        // G the confidence, A the validity. The section 17 texture-channel
        // signature catches the silent layout-mismatch class that
        // motivated this extension (consumers reading `flow_y` from
        // the wrong slot get a structured ChannelMismatch at graph
        // compile time instead of garbage on screen). Consumers that
        // haven't migrated to declare their own typed signature stay
        // wireable through the untyped Texture2D back-compat valve.
        out: Texture2D[R: FLOW_X, G: CONFIDENCE, B: FLOW_Y, A: VALID],
        cut_score: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("analysis_max_dim"),
            label: "Analysis Max Dim",
            ty: ParamType::Int,
            default: ParamValue::Float(360.0),
            range: Some((64.0, 1024.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("update_interval"),
            label: "Update Interval (frames)",
            ty: ParamType::Int,
            default: ParamValue::Float(2.0),
            range: Some((1.0, 30.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("fixed_lag"),
            label: "Fixed Lag",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
    ],
    // depth_rule: output channels encode velocity + confidence + validity, not height — unlike depth_map's literal depth output, flow isn't a meaningful height surrogate
    depth_rule: Terminal,
    composition_notes: "Wire `out` → node.uv_displace_by_flow.flow to advect a source by per-pixel motion (background → particles-along-flow effects, motion-blur-style trails). Wire G channel through node.channel_mixer to use confidence as a mask. Wire `cut_score` → node.filter (threshold ~0.28) → reset_trigger on any downstream stateful primitive to clear frame-to-frame state on hard scene cuts. Until two frames have been inferenced, both outputs are zero. If the native plugin is unavailable, primitive logs a warning once and outputs zero/black. Enable `fixed_lag` when export or a deterministic next-run response deadline matters; it blocks only at that deadline and otherwise keeps one request in flight.",
    examples: [],
    picker: { label: "Optical Flow", category: Atom },
    summary: "Measures how the image is moving between frames and outputs that motion as a flow field. Drive a displace or advect with it to push pixels along the motion.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["optical flow", "optical flow estimate", "motion", "flow", "velocity"],
    boundary_reason: IoBridge,
    extra_fields: {
        upsample_pipeline: Option<GpuComputePipeline> = None,
        flow_worker: Option<BackgroundWorker<FlowRequest, FlowResponse>> = None,
        flow_worker_tried: bool = false,
        flow_state: Option<FlowState> = None,
        generation: u64 = 0,
    },
}

impl OpticalFlowEstimate {
    fn ensure_flow_worker(&mut self) {
        if self.flow_worker.is_some() || self.flow_worker_tried {
            return;
        }
        self.flow_worker_tried = true;
        self.flow_worker = BackgroundWorker::try_new(|| {
            let mut estimator =
                manifold_native::ffi::depth_ffi::FfiDepthEstimator::new_flow_only()?;
            let mut previous: Option<(u64, i32, i32, Vec<u8>)> = None;
            log::info!("[node.optical_flow] Flow worker spawned (Farneback)");
            Some(move |req: FlowRequest| -> FlowResponse {
                let pc = (req.width * req.height) as usize;
                let expected_bytes = pc * 4;
                if req.pixel_data.len() != expected_bytes {
                    return FlowResponse {
                        flow_packed: None,
                        recycled_flow: req.flow_buffer,
                        recycled_pixel_data: req.pixel_data,
                        spare_pixel_data: req.spare_pixel_data,
                        width: req.width,
                        height: req.height,
                        generation: req.generation,
                        analysis_attempted: true,
                        cut_score: 0.0,
                    };
                }
                let current = req.pixel_data;
                let mut recycled_pixel_data = Vec::new();
                let Some((prev_generation, prev_width, prev_height, prev)) = previous.take() else {
                    // First frame: no prev yet, nothing to compute.
                    previous = Some((req.generation, req.width, req.height, current));
                    return FlowResponse {
                        flow_packed: None,
                        recycled_flow: req.flow_buffer,
                        recycled_pixel_data,
                        spare_pixel_data: req.spare_pixel_data,
                        width: req.width,
                        height: req.height,
                        generation: req.generation,
                        analysis_attempted: false,
                        cut_score: 0.0,
                    };
                };
                if prev_generation != req.generation
                    || prev_width != req.width
                    || prev_height != req.height
                {
                    // A clear or analysis resize starts a fresh pair. The
                    // previous packet is still useful to the caller, so
                    // return it rather than cloning or dropping it.
                    recycled_pixel_data = prev;
                    previous = Some((req.generation, req.width, req.height, current));
                    return FlowResponse {
                        flow_packed: None,
                        recycled_flow: req.flow_buffer,
                        recycled_pixel_data,
                        spare_pixel_data: req.spare_pixel_data,
                        width: req.width,
                        height: req.height,
                        generation: req.generation,
                        analysis_attempted: false,
                        cut_score: 0.0,
                    };
                }

                let mut flow = req.flow_buffer;
                flow.resize(pc * 4, 0.0);
                let mut cut_score = [0f32; 1];
                let ok = estimator.compute_flow(
                    &prev,
                    &current,
                    req.width,
                    req.height,
                    &mut flow,
                    req.width,
                    req.height,
                    &mut cut_score,
                );
                let response = if ok != 0 {
                    FlowResponse {
                        flow_packed: Some(flow),
                        recycled_flow: Vec::new(),
                        recycled_pixel_data: prev,
                        spare_pixel_data: req.spare_pixel_data,
                        width: req.width,
                        height: req.height,
                        generation: req.generation,
                        analysis_attempted: true,
                        cut_score: cut_score[0],
                    }
                } else {
                    FlowResponse {
                        flow_packed: None,
                        recycled_flow: flow,
                        recycled_pixel_data: prev,
                        spare_pixel_data: req.spare_pixel_data,
                        width: req.width,
                        height: req.height,
                        generation: req.generation,
                        analysis_attempted: true,
                        cut_score: 0.0,
                    }
                };
                // The current frame becomes the worker's previous frame only
                // after compute_flow has borrowed it.
                previous = Some((req.generation, req.width, req.height, current));
                response
            })
        });
        if self.flow_worker.is_some() {
            record_cold_touch(ColdTouchKind::ModelLoad);
        } else {
            log::warn!(
                "[node.optical_flow] Native flow plugin unavailable — warmup complete; output will be black"
            );
        }
    }

    fn ensure_flow_state(
        &mut self,
        gpu: &mut GpuEncoder,
        width: u32,
        height: u32,
        analysis_max_dim: u32,
    ) {
        let device = gpu.device;
        let max_dim = width.max(height);
        let scale = if max_dim == 0 {
            1.0
        } else {
            (analysis_max_dim as f32 / max_dim as f32).min(1.0)
        };
        let aw = ((width as f32 * scale).round() as u32).max(64);
        let ah = ((height as f32 * scale).round() as u32).max(36);

        let needs_rebuild = match &self.flow_state {
            Some(fs) => fs.analysis_width != aw || fs.analysis_height != ah,
            None => true,
        };
        if !needs_rebuild {
            return;
        }
        if let Some(old) = self.flow_state.as_mut() {
            old.readback.cancel();
        }
        self.generation = self.generation.wrapping_add(1);
        let pixel_count = (aw * ah) as usize;
        let pixel_bytes = pixel_count * 4;
        let flow_texture = device.create_texture(&GpuTextureDesc {
            width: aw,
            height: ah,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
            label: "node.optical_flow.flow",
            mip_levels: 1,
        });
        // Fresh Metal textures have undefined contents and the upsample
        // pass samples this before the first inference arrives — clear so
        // pre-flow output reads zero flow with valid = 0.
        gpu.clear_texture(&flow_texture, 0.0, 0.0, 0.0, 0.0);
        let staging_texture = device.create_texture(&GpuTextureDesc {
            width: aw,
            height: ah,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "node.optical_flow.staging",
            mip_levels: 1,
        });
        self.flow_state = Some(FlowState {
            analysis_width: aw,
            analysis_height: ah,
            readback: ReadbackRequest::new(),
            readback_pending: false,
            has_flow: false,
            flow_dirty: false,
            flow_buffer: vec![0.0f32; pixel_count * 4],
            pixel_buffer: vec![0u8; pixel_bytes],
            spare_pixel_buffer: vec![0u8; pixel_bytes],
            upload_bytes: vec![0u8; pixel_count * 8],
            flow_texture,
            staging_texture,
            last_request_frame: -1024,
            frame_counter: 0,
            cut_score: 0.0,
            first_response_delivered: false,
            warmup_complete: warmup_complete_for(self.flow_worker.is_some(), false),
            analysis_failed: false,
            generation: self.generation,
            clear_texture_pending: false,
        });
    }
}

/// Pack a Vec<f32> of length width*height*4 (R, G, B, A per pixel)
/// into a Vec<u8> of length width*height*8 (4 channels × 2 bytes
/// each as half-floats). The destination texture is Rgba16Float —
/// which is 8 bytes per pixel, NOT 4. (depth_of_field uses 4 here
/// which happens to look OK for low-frequency depth but is
/// technically wrong; for flow's higher precision and signed
/// values we have to do it right.)
fn pack_f32x4_to_rgba16f_bytes(src: &[f32], pixel_count: usize, out: &mut Vec<u8>) {
    debug_assert_eq!(src.len(), pixel_count * 4);
    out.resize(pixel_count * 8, 0);
    for i in 0..pixel_count {
        // Native Farneback layout is [flow_x, flow_y, confidence, valid].
        // The graph convention is [flow_x, confidence, flow_y, valid].
        for (dst_channel, src_channel) in [0usize, 2, 1, 3].into_iter().enumerate() {
            let h = half::f16::from_f32(src[i * 4 + src_channel]);
            let bytes = h.to_le_bytes();
            out[i * 8 + dst_channel * 2] = bytes[0];
            out[i * 8 + dst_channel * 2 + 1] = bytes[1];
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FlowSchedule {
    /// Fixed-lag mode must consume the worker response at the beginning of
    /// this run, before a completed GPU packet is submitted.
    consume_before_submit: bool,
    schedule_readback: bool,
}

fn flow_schedule(
    fixed_lag: bool,
    worker_busy: bool,
    readback_pending: bool,
    readback_due: bool,
    reusable_packet: bool,
) -> FlowSchedule {
    FlowSchedule {
        consume_before_submit: fixed_lag && worker_busy,
        // The GPU cadence is independent of worker readiness in fixed-lag
        // mode. `reusable_packet` is the ownership guard that prevents a
        // second readback from overwriting a packet still held by the worker.
        schedule_readback: readback_due && !readback_pending && reusable_packet,
    }
}

fn flow_response_matches(
    generation: u64,
    width: i32,
    height: i32,
    expected_generation: u64,
    expected_width: u32,
    expected_height: u32,
) -> bool {
    generation == expected_generation
        && width == expected_width as i32
        && height == expected_height as i32
}

fn warmup_complete_for(worker_available: bool, analysis_attempted: bool) -> bool {
    !worker_available || analysis_attempted
}

impl Primitive for OpticalFlowEstimate {
    fn warmup_pending(&self) -> bool {
        self.flow_state.as_ref().is_some_and(|s| !s.warmup_complete)
    }

    fn clear_state(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        let worker_available = self.flow_worker.is_some();
        if let Some(fs) = self.flow_state.as_mut() {
            // The content pipeline reaches clear_state at a frame boundary;
            // cancel releases the old readback without joining the worker.
            fs.readback.cancel();
            fs.readback_pending = false;
            fs.has_flow = false;
            fs.flow_dirty = false;
            fs.cut_score = 0.0;
            fs.first_response_delivered = false;
            fs.warmup_complete = !worker_available;
            fs.analysis_failed = false;
            fs.generation = self.generation;
            fs.clear_texture_pending = true;
            fs.last_request_frame = -1024;
            fs.frame_counter = 0;
            fs.flow_buffer.fill(0.0);
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Always emit cut_score on its scalar port — zero before the
        // first inference completes, latest worker value after. Done
        // before any early-returns so downstream consumers see a
        // usable value even on frames where the texture path is unwired.
        let cut_score = self.flow_state.as_ref().map(|s| s.cut_score).unwrap_or(0.0);
        ctx.outputs
            .set_scalar("cut_score", ParamValue::Float(cut_score));

        let analysis_max_dim = match ctx.params.get("analysis_max_dim") {
            Some(ParamValue::Float(i)) => i.round().clamp(64.0, 1024.0) as u32,
            _ => 360,
        };
        let update_interval = match ctx.params.get("update_interval") {
            Some(ParamValue::Float(i)) => i.round().clamp(1.0, 30.0) as i64,
            _ => 2,
        };
        let fixed_lag = matches!(ctx.params.get("fixed_lag"), Some(ParamValue::Bool(true)));

        let Some(source) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (target.width, target.height);
        if width == 0 || height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        self.ensure_flow_worker();
        self.ensure_flow_state(gpu, source.width, source.height, analysis_max_dim);

        if let Some(fs) = self.flow_state.as_mut()
            && fs.clear_texture_pending
        {
            gpu.clear_texture(&fs.flow_texture, 0.0, 0.0, 0.0, 0.0);
            fs.clear_texture_pending = false;
        }

        if let (Some(fs), Some(fw)) = (self.flow_state.as_mut(), self.flow_worker.as_mut()) {
            // Fixed-lag mode consumes the response at the start of the next
            // run. This is the only blocking point, including export runs;
            // async mode retains the old non-blocking poll.
            let response_schedule = flow_schedule(fixed_lag, fw.is_busy(), false, false, false);
            let response = if response_schedule.consume_before_submit {
                fw.recv_blocking()
            } else {
                fw.try_recv()
            };
            if let Some(response) = response {
                let expected_pixels = (fs.analysis_width * fs.analysis_height * 4) as usize;
                let expected_flow = (fs.analysis_width * fs.analysis_height * 4) as usize;
                let response_matches = flow_response_matches(
                    response.generation,
                    response.width,
                    response.height,
                    fs.generation,
                    fs.analysis_width,
                    fs.analysis_height,
                );

                // Always recycle packet ownership, even for stale responses.
                // A stale result can never update cut_score or the texture.
                for mut packet in [response.recycled_pixel_data, response.spare_pixel_data] {
                    if packet.is_empty() {
                        continue;
                    }
                    packet.resize(expected_pixels, 0);
                    if fs.pixel_buffer.is_empty() {
                        fs.pixel_buffer = packet;
                    } else if fs.spare_pixel_buffer.is_empty() {
                        fs.spare_pixel_buffer = packet;
                    }
                }

                let produced_flow = response.flow_packed.is_some();
                let mut returned_flow = response.flow_packed.or({
                    if response.recycled_flow.is_empty() {
                        None
                    } else {
                        Some(response.recycled_flow)
                    }
                });
                if let Some(mut flow) = returned_flow.take() {
                    flow.resize(expected_flow, 0.0);
                    fs.flow_buffer = flow;
                } else if fs.flow_buffer.is_empty() {
                    fs.flow_buffer.resize(expected_flow, 0.0);
                }

                if response_matches {
                    // cut_score is updated whether or not flow_packed
                    // succeeded — a failed inference legitimately means
                    // "no cut signal this frame," and zero is the right
                    // value for downstream gating.
                    fs.cut_score = response.cut_score;
                    if produced_flow {
                        let first = !fs.has_flow;
                        fs.has_flow = true;
                        fs.flow_dirty = true;
                        fs.first_response_delivered = true;
                        fs.warmup_complete = true;
                        fs.analysis_failed = false;
                        // DIAGNOSTIC: confirm Farneback is producing real motion.
                        // Flow drives mesh advection ("sticking"); all-zero flow
                        // leaves the mesh on identity UVs → grid never tracks the
                        // subject. Packed layout R=flow_x G=conf B=flow_y A=valid.
                        // Log on first arrival, then every ~120 inferences.
                        if first || fs.frame_counter % 120 == 0 {
                            let (mut max_mag, mut max_valid, mut sum_valid) =
                                (0.0f32, 0.0f32, 0.0f32);
                            let px = fs.flow_buffer.len() / 4;
                            for i in 0..px {
                                let fx = fs.flow_buffer[i * 4];
                                let fy = fs.flow_buffer[i * 4 + 1];
                                max_mag = max_mag.max((fx * fx + fy * fy).sqrt());
                                let valid = fs.flow_buffer[i * 4 + 3];
                                max_valid = max_valid.max(valid);
                                sum_valid += valid;
                            }
                            let mean_valid = sum_valid / px.max(1) as f32;
                            log::info!(
                                "[node.optical_flow] flow stats (frame {}): max|flow|={max_mag:.4} \
                             max_valid={max_valid:.3} mean_valid={mean_valid:.3} cut={:.3} \
                             — max|flow|==0 means no motion vectors",
                                fs.frame_counter,
                                fs.cut_score,
                            );
                        }
                    } else if response.analysis_attempted && !fs.first_response_delivered {
                        fs.warmup_complete = warmup_complete_for(true, true);
                        if !fs.analysis_failed {
                            log::warn!(
                                "[node.optical_flow] First Farneback analysis failed — warmup complete; output is zero"
                            );
                        }
                        fs.analysis_failed = true;
                        fs.has_flow = false;
                        fs.flow_buffer.fill(0.0);
                        fs.flow_dirty = true;
                    } else if response.analysis_attempted {
                        if !fs.analysis_failed {
                            log::warn!(
                                "[node.optical_flow] Farneback analysis failed — output is zero"
                            );
                        }
                        fs.analysis_failed = true;
                        fs.has_flow = false;
                        fs.flow_buffer.fill(0.0);
                        fs.flow_dirty = true;
                    }
                }
            }

            // Upload latest flow buffer → analysis-resolution texture.
            if fs.flow_dirty {
                let pixel_count = (fs.analysis_width * fs.analysis_height) as usize;
                pack_f32x4_to_rgba16f_bytes(&fs.flow_buffer, pixel_count, &mut fs.upload_bytes);
                gpu.native_enc.upload_texture(
                    &fs.flow_texture,
                    fs.analysis_width,
                    fs.analysis_height,
                    1,
                    &fs.upload_bytes,
                );
                fs.flow_dirty = false;
            }

            // A completed GPU packet is submitted only after the current
            // flow buffer has been uploaded. This makes the ownership handoff
            // explicit: a buffer moved into the worker can never be read by
            // the upload path in the same run.
            if fs.readback_pending
                && !fw.is_busy()
                && fs.readback.try_read_into(&mut fs.pixel_buffer)
            {
                fs.readback_pending = false;
                let pixels = std::mem::take(&mut fs.pixel_buffer);
                let spare = std::mem::take(&mut fs.spare_pixel_buffer);
                let flow_buffer = std::mem::take(&mut fs.flow_buffer);
                fw.submit(FlowRequest {
                    pixel_data: pixels,
                    spare_pixel_data: spare,
                    flow_buffer,
                    width: fs.analysis_width as i32,
                    height: fs.analysis_height as i32,
                    generation: fs.generation,
                });
            }

            // Submit fresh readback every `update_interval` frames.
            let elapsed = fs.frame_counter - fs.last_request_frame;
            let packet_available =
                fs.pixel_buffer.len() == (fs.analysis_width * fs.analysis_height * 4) as usize;
            let schedule = flow_schedule(
                fixed_lag,
                fw.is_busy(),
                fs.readback.is_pending(),
                elapsed >= update_interval,
                packet_available,
            );
            if schedule.schedule_readback {
                let aw = fs.analysis_width;
                let ah = fs.analysis_height;
                // Bilinear downscale of the WHOLE source into the cached
                // analysis-res staging — NOT a blit. A same-size blit
                // would crop the top-left corner, so the flow net would
                // only ever see motion in ~9% of a 4K frame. See
                // GpuEncoder::resize_sample. resize_sample fully overwrites the
                // staging and submit copies it into its own buffer, so reusing
                // the cached texture across cadences is safe (a new submit only
                // runs once the prior readback completed — !is_pending guard).
                gpu.resize_sample(source, &fs.staging_texture);
                fs.readback.submit(gpu, &fs.staging_texture, aw, ah);
                fs.readback_pending = true;
                fs.last_request_frame = fs.frame_counter;
            }
            fs.frame_counter += 1;
        }

        // Always run the upsample pass — empty flow_texture → black output.
        let pipeline = self.upsample_pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/optical_flow_estimate_upsample.wgsl"),
                "cs_main",
                "node.optical_flow.upsample",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let Some(flow_state) = self.flow_state.as_ref() else {
            return;
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture: &flow_state.flow_texture,
                },
                GpuBinding::Sampler {
                    binding: 1,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: target,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.optical_flow",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn optical_flow_estimate_declares_one_input_and_two_outputs() {
        use crate::node_graph::channel_names::well_known;
        use crate::node_graph::ports::{PortType, ScalarType, TextureChannels};
        assert_eq!(OpticalFlowEstimate::TYPE_ID, "node.optical_flow");
        assert_eq!(OpticalFlowEstimate::INPUTS.len(), 1);
        assert_eq!(OpticalFlowEstimate::INPUTS[0].name, "in");
        assert_eq!(OpticalFlowEstimate::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(OpticalFlowEstimate::OUTPUTS.len(), 2);
        assert_eq!(OpticalFlowEstimate::OUTPUTS[0].name, "out");
        // The output declares the Watercolor RGBA layout per section 17 so
        // any consumer that has also migrated to a typed Texture2D
        // signature gets a structured ChannelMismatch on layout drift.
        assert_eq!(
            OpticalFlowEstimate::OUTPUTS[0].ty,
            PortType::Texture2DTyped(TextureChannels::new(
                well_known::FLOW_X,
                well_known::CONFIDENCE,
                well_known::FLOW_Y,
                well_known::VALID,
            ))
        );
        assert_eq!(OpticalFlowEstimate::OUTPUTS[1].name, "cut_score");
        assert_eq!(
            OpticalFlowEstimate::OUTPUTS[1].ty,
            PortType::Scalar(ScalarType::F32)
        );
    }

    #[test]
    fn optical_flow_estimate_has_analysis_and_interval_params() {
        let names: Vec<&str> = OpticalFlowEstimate::PARAMS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(
            names,
            vec!["analysis_max_dim", "update_interval", "fixed_lag"]
        );
        assert_eq!(
            OpticalFlowEstimate::PARAMS[2].default,
            ParamValue::Bool(false)
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = OpticalFlowEstimate::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.optical_flow");
    }

    #[test]
    fn pack_f32_to_rgba16f_preserves_4_channels() {
        let src = vec![0.0_f32, 0.25, 0.5, 1.0];
        let mut bytes = Vec::new();
        pack_f32x4_to_rgba16f_bytes(&src, 1, &mut bytes);
        assert_eq!(bytes.len(), 8);
        let r = half::f16::from_le_bytes([bytes[0], bytes[1]]).to_f32();
        let confidence = half::f16::from_le_bytes([bytes[2], bytes[3]]).to_f32();
        let y = half::f16::from_le_bytes([bytes[4], bytes[5]]).to_f32();
        let a = half::f16::from_le_bytes([bytes[6], bytes[7]]).to_f32();
        assert!((r - 0.0).abs() < 0.01);
        assert!((confidence - 0.5).abs() < 0.01);
        assert!((y - 0.25).abs() < 0.01);
        assert!((a - 1.0).abs() < 0.01);
    }

    #[test]
    fn stale_flow_response_is_rejected_by_generation_and_dimensions() {
        assert!(flow_response_matches(7, 64, 36, 7, 64, 36));
        assert!(!flow_response_matches(6, 64, 36, 7, 64, 36));
        assert!(!flow_response_matches(7, 128, 36, 7, 64, 36));
    }

    #[test]
    fn unavailable_or_failed_first_analysis_finishes_warmup() {
        assert!(warmup_complete_for(false, false));
        assert!(warmup_complete_for(true, true));
        assert!(!warmup_complete_for(true, false));
    }

    #[test]
    fn fixed_lag_consumes_before_submitting_and_keeps_readback_cadence() {
        let step = flow_schedule(true, true, false, true, true);
        assert!(step.consume_before_submit);
        assert!(step.schedule_readback);

        let blocked = flow_schedule(true, true, true, true, true);
        assert!(blocked.consume_before_submit);
        assert!(!blocked.schedule_readback);

        let async_step = flow_schedule(false, true, false, true, true);
        assert!(!async_step.consume_before_submit);
        assert!(async_step.schedule_readback);
    }
}
