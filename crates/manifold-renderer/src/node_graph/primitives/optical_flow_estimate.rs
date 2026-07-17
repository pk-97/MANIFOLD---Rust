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

#![allow(private_interfaces)]

use std::borrow::Cow;

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
    width: i32,
    height: i32,
}

struct FlowResponse {
    /// Packed [flow_x, flow_y, confidence, valid_mask] per pixel,
    /// length = width * height * 4. None on first frame (no prev),
    /// model failure, or invalid dims.
    flow_packed: Option<Vec<f32>>,
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
        // G the confidence, A the validity. The §17 texture-channel
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
    ],
    // depth_rule: output channels encode velocity + confidence + validity, not height — unlike depth_map's literal depth output, flow isn't a meaningful height surrogate
    depth_rule: Terminal,
    composition_notes: "Wire `out` → node.uv_displace_by_flow.flow to advect a source by per-pixel motion (background → particles-along-flow effects, motion-blur-style trails). Wire G channel through node.channel_mixer to use confidence as a mask. Wire `cut_score` → node.filter (threshold ~0.28) → reset_trigger on any downstream stateful primitive to clear frame-to-frame state on hard scene cuts. Until two frames have been inferenced, both outputs are zero. If the native plugin is unavailable, primitive logs a warning once and outputs zero/black.",
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
            let mut prev_frame: Option<Vec<u8>> = None;
            log::info!("[node.optical_flow] Flow worker spawned (Farneback)");
            Some(move |req: FlowRequest| -> FlowResponse {
                let pc = (req.width * req.height) as usize;
                let expected_bytes = pc * 4;
                if req.pixel_data.len() != expected_bytes {
                    return FlowResponse {
                        flow_packed: None,
                        cut_score: 0.0,
                    };
                }
                let curr = req.pixel_data;
                let Some(prev) = prev_frame.replace(curr.clone()) else {
                    // First frame: no prev yet, nothing to compute.
                    return FlowResponse {
                        flow_packed: None,
                        cut_score: 0.0,
                    };
                };
                let mut flow = vec![0f32; pc * 4];
                let mut cut_score = [0f32; 1];
                let ok = estimator.compute_flow(
                    &prev,
                    &curr,
                    req.width,
                    req.height,
                    &mut flow,
                    req.width,
                    req.height,
                    &mut cut_score,
                );
                if ok != 0 {
                    FlowResponse {
                        flow_packed: Some(flow),
                        cut_score: cut_score[0],
                    }
                } else {
                    FlowResponse {
                        flow_packed: None,
                        cut_score: 0.0,
                    }
                }
            })
        });
        if self.flow_worker.is_none() {
            log::warn!(
                "[node.optical_flow] Native flow plugin unavailable — output will be black"
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
        let pixel_count = (aw * ah) as usize;
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
            flow_texture,
            staging_texture,
            last_request_frame: -1024,
            frame_counter: 0,
            cut_score: 0.0,
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
fn pack_f32x4_to_rgba16f_bytes(src: &[f32], pixel_count: usize) -> Vec<u8> {
    debug_assert_eq!(src.len(), pixel_count * 4);
    let mut out = vec![0u8; pixel_count * 8];
    for i in 0..pixel_count {
        for c in 0..4 {
            let h = half::f16::from_f32(src[i * 4 + c]);
            let bytes = h.to_le_bytes();
            out[i * 8 + c * 2] = bytes[0];
            out[i * 8 + c * 2 + 1] = bytes[1];
        }
    }
    out
}

impl Primitive for OpticalFlowEstimate {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Always emit cut_score on its scalar port — zero before the
        // first inference completes, latest worker value after. Done
        // before any early-returns so downstream consumers see a
        // usable value even on frames where the texture path is unwired.
        let cut_score = self.flow_state.as_ref().map(|s| s.cut_score).unwrap_or(0.0);
        ctx.outputs
            .set_scalar("cut_score", ParamValue::Float(cut_score));

        let analysis_max_dim = match ctx.params.get("analysis_max_dim") {
            Some(ParamValue::Float(i)) => i.round().max(64_f32) as u32,
            _ => 360,
        };
        let update_interval = match ctx.params.get("update_interval") {
            Some(ParamValue::Float(i)) => i.round().max(1_f32) as i64,
            _ => 2,
        };

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

        if let (Some(fs), Some(fw)) = (self.flow_state.as_mut(), self.flow_worker.as_mut()) {
            // Poll readback → submit to worker (which holds prev frame).
            if fs.readback_pending
                && let Some(pixels) = fs.readback.try_read()
            {
                fs.readback_pending = false;
                fw.submit(FlowRequest {
                    pixel_data: pixels,
                    width: fs.analysis_width as i32,
                    height: fs.analysis_height as i32,
                });
            }

            // Poll worker result → mark flow_dirty.
            if let Some(response) = fw.try_recv() {
                // cut_score is updated whether or not flow_packed
                // succeeded — a failed inference legitimately means
                // "no cut signal this frame," and zero is the right
                // value for downstream gating.
                fs.cut_score = response.cut_score;
                if let Some(buf) = response.flow_packed {
                    let first = !fs.has_flow;
                    fs.flow_buffer = buf;
                    fs.has_flow = true;
                    fs.flow_dirty = true;
                    // DIAGNOSTIC: confirm Farneback is producing real motion.
                    // Flow drives mesh advection ("sticking"); all-zero flow
                    // leaves the mesh on identity UVs → grid never tracks the
                    // subject. Packed layout R=flow_x G=conf B=flow_y A=valid.
                    // Log on first arrival, then every ~120 inferences.
                    if first || fs.frame_counter % 120 == 0 {
                        let (mut max_mag, mut max_valid, mut sum_valid) = (0.0f32, 0.0f32, 0.0f32);
                        let px = fs.flow_buffer.len() / 4;
                        for i in 0..px {
                            let fx = fs.flow_buffer[i * 4];
                            let fy = fs.flow_buffer[i * 4 + 2];
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
                            fs.frame_counter, fs.cut_score,
                        );
                    }
                }
            }

            // Upload latest flow buffer → analysis-resolution texture.
            if fs.flow_dirty {
                let pixel_count = (fs.analysis_width * fs.analysis_height) as usize;
                // FFI returns packed [flow_x, flow_y, confidence, valid_mask]
                // per pixel. Re-pack to R=flow_x, G=confidence, B=flow_y, A=valid
                // so this composes with the Watercolor R/B-flow convention.
                let mut reordered = vec![0f32; pixel_count * 4];
                for i in 0..pixel_count {
                    let fx = fs.flow_buffer[i * 4];
                    let fy = fs.flow_buffer[i * 4 + 1];
                    let conf = fs.flow_buffer[i * 4 + 2];
                    let valid = fs.flow_buffer[i * 4 + 3];
                    reordered[i * 4] = fx;
                    reordered[i * 4 + 1] = conf;
                    reordered[i * 4 + 2] = fy;
                    reordered[i * 4 + 3] = valid;
                }
                let bytes = pack_f32x4_to_rgba16f_bytes(&reordered, pixel_count);
                gpu.native_enc.upload_texture(
                    &fs.flow_texture,
                    fs.analysis_width,
                    fs.analysis_height,
                    1,
                    &bytes,
                );
                fs.flow_dirty = false;
            }

            // Submit fresh readback every `update_interval` frames.
            let elapsed = fs.frame_counter - fs.last_request_frame;
            if elapsed >= update_interval && !fs.readback.is_pending() {
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
        // The output declares the Watercolor RGBA layout per §17 so
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
        assert_eq!(names, vec!["analysis_max_dim", "update_interval"]);
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
        let bytes = pack_f32x4_to_rgba16f_bytes(&src, 1);
        assert_eq!(bytes.len(), 8);
        let r = half::f16::from_le_bytes([bytes[0], bytes[1]]).to_f32();
        let g = half::f16::from_le_bytes([bytes[2], bytes[3]]).to_f32();
        let b = half::f16::from_le_bytes([bytes[4], bytes[5]]).to_f32();
        let a = half::f16::from_le_bytes([bytes[6], bytes[7]]).to_f32();
        assert!((r - 0.0).abs() < 0.01);
        assert!((g - 0.25).abs() < 0.01);
        assert!((b - 0.5).abs() < 0.01);
        assert!((a - 1.0).abs() < 0.01);
    }
}
