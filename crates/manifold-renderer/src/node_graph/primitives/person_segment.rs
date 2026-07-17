//! `node.person_mask` — person/human segmentation via the native
//! plugin's `process_subject_mask` API, wrapped as a standalone
//! primitive.
//!
//! The underlying ONNX model is a person/selfie segmentation model
//! (selfie_segmentation_256.onnx / human_segmentation_256.onnx /
//! person_segment_lite.onnx — whichever the plugin finds first; see
//! BuildSubjectModelCandidates in DepthEstimatorPlugin.cpp). Detects
//! PEOPLE specifically, not arbitrary salient objects.
//!
//! Output: Rgba16Float texture where R = G = B = person probability
//! (0 = background, 1 = person; bilinear-upsampled from analysis
//! resolution). A is the availability gate: 0 until the first
//! inference completes, 1 afterwards — consumers check it to tell a
//! real "no person" mask apart from "no mask yet". R/G/B layout
//! matches `depth_estimate_midas` so they compose interchangeably
//! as mask inputs.
//!
//! Temporal blending (worker-side, α = 0.55 by default matching the
//! legacy WireframeDepth contract) reduces noise *before* the GPU
//! upload. The worker holds the previous blended buffer internally;
//! when the buffer is fresh the first inference passes through
//! unblended (no bleed from zero).
//!
//! Inference runs on a background worker thread with ~2-3 frame
//! latency (same shape as `depth_estimate_midas` / `optical_flow_estimate`).
//! Until the first inference completes, output is black.

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

struct MaskRequest {
    pixel_data: Vec<u8>,
    width: i32,
    height: i32,
    /// Temporal blend coefficient, passed per-request so live param
    /// changes take effect on the next inference rather than being
    /// baked at worker spawn.
    smoothing: f32,
}

struct MaskResponse {
    /// Temporally-blended mask buffer (worker-side blend already
    /// applied). `None` if the FFI call returned a failure code.
    mask_blended: Option<Vec<f32>>,
}

struct MaskState {
    analysis_width: u32,
    analysis_height: u32,
    readback: ReadbackRequest,
    readback_pending: bool,
    has_mask: bool,
    mask_dirty: bool,
    mask_buffer: Vec<f32>,
    mask_texture: GpuTexture,
    /// Analysis-res downscale target for the readback. Cached here (rebuilt only
    /// when analysis dims change) so `run` never allocates per readback cadence.
    staging_texture: GpuTexture,
    last_request_frame: i64,
    frame_counter: i64,
}

crate::primitive! {
    name: PersonSegment,
    type_id: "node.person_mask",
    purpose: "Person / human segmentation via the native plugin's process_subject_mask API. Detects PEOPLE specifically (selfie / human / person model variants), not generic salient objects. Input: any Texture2D frame. Output: Rgba16Float mask where R = G = B = person probability ∈ [0, 1] (0 = background, 1 = person); A = 0 until the first inference completes, 1 afterwards (availability gate). Inference runs on a background worker with ~2-3 frame latency. Temporal blending (α = 0.55 default) reduces noise worker-side before upload — matches the legacy WireframeDepth contract. Same channel pack as depth_estimate_midas so they compose interchangeably as mask inputs.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
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
            name: Cow::Borrowed("smoothing"),
            label: "Temporal Smoothing",
            ty: ParamType::Float,
            // Matches legacy WireframeDepth's hardcoded BLEND = 0.55
            // for the per-frame mask history mix
            // (`hist[i] += (mask[i] - hist[i]) * BLEND`). Lowering
            // gives more reactive but noisier masks; raising gives
            // smoother but laggier ones.
            default: ParamValue::Float(0.55),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    // depth_rule: DNN segmentation mask, coincident per-pixel output like chroma_key's keying role, despite a neighborhood-spanning receptive field
    depth_rule: Inherit,
    composition_notes: "Wire output → node.compose `mask` input (or `node.masked_mix`) to apply effects selectively to people vs background. Combine with depth_estimate_midas via node.mix Multiply for depth-AND-person-gated isolation. Lower analysis_max_dim for faster inference at coarser masks; higher update_interval reduces CPU load at the cost of temporal lag. smoothing controls worker-side temporal blend — α = 0.55 matches the legacy WireframeDepth behavior. If the native plugin's subject API is unavailable (older plugin builds without the segmentation model), logs a warning once and outputs black.",
    examples: [],
    picker: { label: "Person Mask", category: Atom },
    summary: "Finds people in the image with an AI model and outputs a mask that is white on the person and black elsewhere. Use it to cut someone out or key effects to them.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["person mask", "person segment", "segmentation", "people", "matte"],
    boundary_reason: IoBridge,
    extra_fields: {
        upsample_pipeline: Option<GpuComputePipeline> = None,
        mask_worker: Option<BackgroundWorker<MaskRequest, MaskResponse>> = None,
        mask_worker_tried: bool = false,
        mask_state: Option<MaskState> = None,
    },
}

impl PersonSegment {
    fn ensure_mask_worker(&mut self) {
        if self.mask_worker.is_some() || self.mask_worker_tried {
            return;
        }
        self.mask_worker_tried = true;
        self.mask_worker = BackgroundWorker::try_new(|| {
            let mut estimator =
                manifold_native::ffi::depth_ffi::FfiDepthEstimator::new_subject_only()?;
            // Worker holds the previous blended buffer internally.
            // First inference: pass-through (no prev → blended = clamped
            // mask). Subsequent: hist += (curr - hist) * smoothing.
            let mut prev_blended: Option<Vec<f32>> = None;
            log::info!("[node.person_mask] Person-segmentation worker spawned");
            Some(move |req: MaskRequest| -> MaskResponse {
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
                if ok == 0 {
                    return MaskResponse { mask_blended: None };
                }
                // α = req.smoothing, default 0.55 matches legacy
                // WireframeDepth's BLEND constant exactly.
                let alpha = req.smoothing.clamp(0.0, 1.0);
                let blended: Vec<f32> = match prev_blended.take() {
                    Some(mut hist) if hist.len() == pc => {
                        for i in 0..pc {
                            hist[i] = hist[i] + (mask[i].clamp(0.0, 1.0) - hist[i]) * alpha;
                        }
                        hist
                    }
                    _ => mask.iter().map(|v| v.clamp(0.0, 1.0)).collect(),
                };
                prev_blended = Some(blended.clone());
                MaskResponse {
                    mask_blended: Some(blended),
                }
            })
        });
        if self.mask_worker.is_none() {
            log::warn!(
                "[node.person_mask] Native subject-segmentation API unavailable — output will be black"
            );
        }
    }

    fn ensure_mask_state(
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

        let needs_rebuild = match &self.mask_state {
            Some(ms) => ms.analysis_width != aw || ms.analysis_height != ah,
            None => true,
        };
        if !needs_rebuild {
            return;
        }
        let pixel_count = (aw * ah) as usize;
        // Rgba8Unorm to match the u8 scalar pack in run(). upload_texture
        // derives bytesPerRow from the texture FORMAT, so a wider format
        // here makes Metal reinterpret the u8 rows as f16 garbage.
        let mask_texture = device.create_texture(&GpuTextureDesc {
            width: aw,
            height: ah,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
            label: "node.person_mask.mask",
            mip_levels: 1,
        });
        // Clear so alpha reads 0 until the first real mask upload — the
        // upsample pass forwards this alpha as the "DNN mask available"
        // gate consumers like WireframeDepthGraph's wire pass rely on.
        gpu.clear_texture(&mask_texture, 0.0, 0.0, 0.0, 0.0);
        let staging_texture = device.create_texture(&GpuTextureDesc {
            width: aw,
            height: ah,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "node.person_mask.staging",
            mip_levels: 1,
        });
        self.mask_state = Some(MaskState {
            analysis_width: aw,
            analysis_height: ah,
            readback: ReadbackRequest::new(),
            readback_pending: false,
            has_mask: false,
            mask_dirty: false,
            mask_buffer: vec![0.0f32; pixel_count],
            mask_texture,
            staging_texture,
            last_request_frame: -1024,
            frame_counter: 0,
        });
    }
}

impl Primitive for PersonSegment {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let analysis_max_dim = match ctx.params.get("analysis_max_dim") {
            Some(ParamValue::Float(i)) => i.round().max(64_f32) as u32,
            _ => 360,
        };
        let update_interval = match ctx.params.get("update_interval") {
            Some(ParamValue::Float(i)) => i.round().max(1_f32) as i64,
            _ => 2,
        };
        let smoothing = match ctx.params.get("smoothing") {
            Some(ParamValue::Float(f)) => f.clamp(0.0, 1.0),
            _ => 0.55,
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
        self.ensure_mask_worker();
        self.ensure_mask_state(gpu, source.width, source.height, analysis_max_dim);

        if let (Some(ms), Some(mw)) = (self.mask_state.as_mut(), self.mask_worker.as_mut()) {
            // Poll readback → submit to worker.
            if ms.readback_pending
                && let Some(pixels) = ms.readback.try_read()
            {
                ms.readback_pending = false;
                mw.submit(MaskRequest {
                    pixel_data: pixels,
                    width: ms.analysis_width as i32,
                    height: ms.analysis_height as i32,
                    smoothing,
                });
            }

            // Poll worker result → mark mask_dirty.
            if let Some(response) = mw.try_recv()
                && let Some(buf) = response.mask_blended
            {
                let first = !ms.has_mask;
                ms.mask_buffer = buf;
                ms.has_mask = true;
                ms.mask_dirty = true;
                // DIAGNOSTIC: confirm the segmentation model returns a real
                // subject mask. An all-zero mask makes the subject-isolation
                // path in wire_mask fall back to depth/semantic heuristics.
                // Log on first arrival, then every ~120 inferences.
                if first || ms.frame_counter % 120 == 0 {
                    let (mut mx, mut sum) = (0.0f32, 0.0f32);
                    for &v in &ms.mask_buffer {
                        mx = mx.max(v);
                        sum += v;
                    }
                    let mean = sum / ms.mask_buffer.len().max(1) as f32;
                    log::info!(
                        "[node.person_mask] mask stats (frame {}): max={mx:.3} mean={mean:.3} \
                         — max==0 means no subject mask",
                        ms.frame_counter,
                    );
                }
            }

            // Upload latest mask buffer → analysis-resolution texture.
            // Same Rgba8Unorm scalar pack as depth_estimate_midas so
            // the upsample shader can sample .r and broadcast.
            if ms.mask_dirty {
                let count = (ms.analysis_width * ms.analysis_height) as usize;
                let mut pixels = vec![0u8; count * 4];
                for i in 0..count {
                    let v = (ms.mask_buffer[i].clamp(0.0, 1.0) * 255.0) as u8;
                    pixels[i * 4] = v;
                    pixels[i * 4 + 1] = v;
                    pixels[i * 4 + 2] = v;
                    pixels[i * 4 + 3] = 255;
                }
                gpu.native_enc.upload_texture(
                    &ms.mask_texture,
                    ms.analysis_width,
                    ms.analysis_height,
                    1,
                    &pixels,
                );
                ms.mask_dirty = false;
            }

            // Submit fresh readback every `update_interval` frames.
            let elapsed = ms.frame_counter - ms.last_request_frame;
            if elapsed >= update_interval && !ms.readback.is_pending() {
                let aw = ms.analysis_width;
                let ah = ms.analysis_height;
                // Bilinear downscale of the WHOLE source into the cached
                // analysis-res staging — NOT a blit. A same-size blit
                // would crop the top-left corner, so segmentation would
                // only see ~9% of a 4K frame. See GpuEncoder::resize_sample.
                // resize_sample fully overwrites the staging and submit copies
                // it into its own buffer, so reusing the cached texture across
                // cadences is safe (a new submit only runs once the prior
                // readback completed — guarded by !ms.readback.is_pending()).
                gpu.resize_sample(source, &ms.staging_texture);
                ms.readback.submit(gpu, &ms.staging_texture, aw, ah);
                ms.readback_pending = true;
                ms.last_request_frame = ms.frame_counter;
            }
            ms.frame_counter += 1;
        }

        // Always run the upsample pass — empty mask_texture → black output.
        let pipeline = self.upsample_pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/person_segment_upsample.wgsl"),
                "cs_main",
                "node.person_mask.upsample",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let Some(mask_state) = self.mask_state.as_ref() else {
            return;
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture: &mask_state.mask_texture,
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
            "node.person_mask",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn person_segment_declares_one_input_and_one_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(PersonSegment::TYPE_ID, "node.person_mask");
        assert_eq!(PersonSegment::INPUTS.len(), 1);
        assert_eq!(PersonSegment::INPUTS[0].name, "in");
        assert_eq!(PersonSegment::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(PersonSegment::OUTPUTS.len(), 1);
        assert_eq!(PersonSegment::OUTPUTS[0].name, "out");
        assert_eq!(PersonSegment::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn person_segment_has_three_params() {
        let names: Vec<&str> = PersonSegment::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["analysis_max_dim", "update_interval", "smoothing"]);
    }

    #[test]
    fn person_segment_smoothing_defaults_to_legacy_055() {
        // Matches legacy WireframeDepth's hardcoded BLEND = 0.55 for
        // the subject-mask temporal history mix. Value-level parity
        // gate — if this drifts the WireframeDepth decomposition's
        // mask convergence rate diverges from the original.
        let smoothing_param = PersonSegment::PARAMS
            .iter()
            .find(|p| p.name == "smoothing")
            .expect("smoothing param exists");
        if let ParamValue::Float(f) = smoothing_param.default {
            assert!(
                (f - 0.55).abs() < 1e-6,
                "smoothing default must be 0.55 to match legacy, got {f}",
            );
        } else {
            panic!("smoothing default must be a Float");
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = PersonSegment::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.person_mask");
    }
}
