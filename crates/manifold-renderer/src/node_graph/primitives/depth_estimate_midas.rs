//! `node.depth_map` — monocular depth estimation via the
//! MiDaS DNN, exposed as a standalone primitive.
//!
//! Note on `#![allow(private_interfaces)]`: the `primitive!` macro
//! emits the `extra_fields:` we declare as `pub`, but the request/
//! response/state types they hold are intentionally module-private
//! implementation details. Silenced here rather than leaking those
//! types into the crate-public surface.

#![allow(private_interfaces)]
//!
//! Wraps `manifold_native::ffi::depth_ffi::FfiDepthEstimator` on a
//! background worker thread so the content thread never blocks. The
//! input frame is downsampled and read back to a CPU buffer (async),
//! inferenced, then uploaded back as a small depth staging texture
//! and bilinear-upsampled into the runtime-allocated output port.
//!
//! Output: Rgba16Float texture where R = G = B = depth (normalized
//! 0..1; near = 1, far = 0 in MiDaS convention), A = 1.
//!
//! Same readback / worker / upload pattern used by
//! `node.depth_of_field`'s depth focus mode — extracted here so any
//! graph can drive any downstream effect from a depth signal (e.g.
//! split a video into depth-based layers and apply different
//! filters per range; convert the background by depth to particles
//! that flow on optical flow; etc.).

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

struct DepthRequest {
    pixel_data: Vec<u8>,
    width: i32,
    height: i32,
}

struct DepthResponse {
    depth_buffer: Option<Vec<f32>>,
    // Dims the depth was computed at. Used to discard responses that arrive
    // after the analysis resolution changed (in-flight request raced a
    // source-size switch) — otherwise the upload loop indexes a stale,
    // smaller buffer with the new, larger pixel count and panics.
    width: i32,
    height: i32,
}

struct DepthState {
    analysis_width: u32,
    analysis_height: u32,
    readback: ReadbackRequest,
    readback_pending: bool,
    has_depth: bool,
    depth_dirty: bool,
    depth_buffer: Vec<f32>,
    depth_texture: GpuTexture,
    /// Analysis-res downscale target for the readback. Cached here (rebuilt only
    /// when analysis dims change) so `run` never allocates per readback cadence.
    staging_texture: GpuTexture,
    last_request_frame: i64,
    frame_counter: i64,
}

crate::primitive! {
    name: DepthEstimateMidas,
    type_id: "node.depth_map",
    purpose: "MiDaS monocular depth estimation via FFI native plugin, wrapped as a primitive. Input: any Texture2D frame. Output: depth map (R = G = B = depth ∈ [0, 1], near = 1, far = 0; A = 1). Inference runs on a background worker thread with ~2-3 frame latency; output is bilinear-upsampled from an analysis-resolution staging texture into the runtime-allocated output. Until first inference completes, the output is black.",
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
    ],
    depth_rule: SourceHeight,
    composition_notes: "If the MiDaS native plugin can't be loaded, the primitive logs a warning once and outputs black thereafter. Lower analysis_max_dim makes inference faster but coarser; higher update_interval reduces CPU load at the cost of temporal lag. Compose into node.compose with a luminance mask to layer depth-aware effects, or feed the depth into another primitive that accepts a control texture.",
    examples: [],
    picker: { label: "Depth Map", category: Atom },
    summary: "Estimates a depth map from any flat image with an AI model, so nearer things read bright and far things dark. Feed it into a blur or displace to fake 3D from 2D footage.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["depth map", "depth estimate midas", "midas", "depth", "ai depth"],
    boundary_reason: IoBridge,
    extra_fields: {
        upsample_pipeline: Option<GpuComputePipeline> = None,
        depth_worker: Option<BackgroundWorker<DepthRequest, DepthResponse>> = None,
        depth_worker_tried: bool = false,
        depth_state: Option<DepthState> = None,
    },
}

impl DepthEstimateMidas {
    fn ensure_depth_worker(&mut self) {
        if self.depth_worker.is_some() || self.depth_worker_tried {
            return;
        }
        self.depth_worker_tried = true;
        self.depth_worker = BackgroundWorker::try_new(|| {
            let mut estimator =
                manifold_native::ffi::depth_ffi::FfiDepthEstimator::new_depth_only()?;
            log::info!("[node.depth_map] MiDaS worker spawned (depth-only)");
            Some(move |req: DepthRequest| -> DepthResponse {
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
                DepthResponse {
                    depth_buffer: if ok != 0 { Some(depth) } else { None },
                    width: req.width,
                    height: req.height,
                }
            })
        });
        if self.depth_worker.is_none() {
            log::warn!(
                "[node.depth_map] MiDaS native plugin unavailable — output will be black"
            );
        }
    }

    fn ensure_depth_state(
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

        // Re-create the staging texture if analysis dims change
        // (e.g. caller switched analysis_max_dim or input size).
        let needs_rebuild = match &self.depth_state {
            Some(ds) => ds.analysis_width != aw || ds.analysis_height != ah,
            None => true,
        };
        if !needs_rebuild {
            return;
        }
        let pixel_count = (aw * ah) as usize;
        // Rgba8Unorm to match the u8 scalar pack in run(). upload_texture
        // derives bytesPerRow from the texture FORMAT, so a wider format
        // here makes Metal reinterpret the u8 rows as f16 garbage (this
        // was the dead-Z-slider bug in WireframeDepthGraph).
        let depth_texture = device.create_texture(&GpuTextureDesc {
            width: aw,
            height: ah,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
            label: "node.depth_map.depth",
            mip_levels: 1,
        });
        // Fresh Metal textures have undefined contents, and the upsample
        // pass samples this before the first inference arrives.
        gpu.clear_texture(&depth_texture, 0.0, 0.0, 0.0, 0.0);
        let staging_texture = device.create_texture(&GpuTextureDesc {
            width: aw,
            height: ah,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "node.depth_map.staging",
            mip_levels: 1,
        });
        self.depth_state = Some(DepthState {
            analysis_width: aw,
            analysis_height: ah,
            readback: ReadbackRequest::new(),
            readback_pending: false,
            has_depth: false,
            depth_dirty: false,
            depth_buffer: vec![0.0f32; pixel_count],
            depth_texture,
            staging_texture,
            last_request_frame: -1024,
            frame_counter: 0,
        });
    }
}

impl Primitive for DepthEstimateMidas {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
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
        self.ensure_depth_worker();
        self.ensure_depth_state(gpu, source.width, source.height, analysis_max_dim);

        if let (Some(ds), Some(dw)) = (self.depth_state.as_mut(), self.depth_worker.as_mut()) {
            // Poll readback → submit to worker.
            if ds.readback_pending
                && let Some(pixels) = ds.readback.try_read()
            {
                ds.readback_pending = false;
                dw.submit(DepthRequest {
                    pixel_data: pixels,
                    width: ds.analysis_width as i32,
                    height: ds.analysis_height as i32,
                });
            }

            // Poll worker result → mark depth_dirty.
            if let Some(response) = dw.try_recv()
                && let Some(buf) = response.depth_buffer
                // Discard a response computed at dims that no longer match the
                // current analysis resolution (source size changed while it was
                // in flight). A fresh request at the new dims is already queued.
                && response.width as u32 == ds.analysis_width
                && response.height as u32 == ds.analysis_height
            {
                let first = !ds.has_depth;
                ds.depth_buffer = buf;
                ds.has_depth = true;
                ds.depth_dirty = true;
                // DIAGNOSTIC: confirm MiDaS is producing real (non-flat)
                // depth. An all-zero / constant buffer downstream makes
                // `z = depth * depth_scale` collapse → the Wireframe Depth
                // Z-slider does nothing and the grid never gains 3D. Log on
                // first arrival, then every ~120 inferences.
                if first || ds.frame_counter % 120 == 0 {
                    let (mut mn, mut mx, mut sum) = (f32::INFINITY, f32::NEG_INFINITY, 0.0f32);
                    for &v in &ds.depth_buffer {
                        mn = mn.min(v);
                        mx = mx.max(v);
                        sum += v;
                    }
                    let mean = sum / ds.depth_buffer.len().max(1) as f32;
                    log::info!(
                        "[node.depth_map] depth stats (frame {}): min={mn:.3} max={mx:.3} mean={mean:.3} \
                         — near=1/far=0; min==max==0 means MiDaS returned no depth",
                        ds.frame_counter,
                    );
                }
            }

            // Upload latest depth buffer → analysis-resolution texture.
            if ds.depth_dirty {
                let count = (ds.analysis_width * ds.analysis_height) as usize;
                let mut pixels = vec![0u8; count * 4];
                for i in 0..count {
                    let v = (ds.depth_buffer[i].clamp(0.0, 1.0) * 255.0) as u8;
                    pixels[i * 4] = v;
                    pixels[i * 4 + 1] = v;
                    pixels[i * 4 + 2] = v;
                    pixels[i * 4 + 3] = 255;
                }
                gpu.native_enc.upload_texture(
                    &ds.depth_texture,
                    ds.analysis_width,
                    ds.analysis_height,
                    1,
                    &pixels,
                );
                ds.depth_dirty = false;
            }

            // Submit fresh readback every `update_interval` frames.
            let elapsed = ds.frame_counter - ds.last_request_frame;
            if elapsed >= update_interval && !ds.readback.is_pending() {
                let aw = ds.analysis_width;
                let ah = ds.analysis_height;
                // Bilinear downscale of the WHOLE source into the cached
                // analysis-res staging — NOT a blit. A same-size blit
                // (copy_texture_to_texture) would crop the top-left
                // analysis-sized corner of the full-res frame, so MiDaS
                // would estimate depth on ~9% of a 4K image (flat corner
                // → dead Z-slider downstream). See GpuEncoder::resize_sample.
                // resize_sample fully overwrites the staging and submit copies
                // it into its own buffer, so reusing the cached texture across
                // cadences is safe (a new submit only runs once the prior
                // readback completed — guarded by !ds.readback.is_pending()).
                gpu.resize_sample(source, &ds.staging_texture);
                ds.readback.submit(gpu, &ds.staging_texture, aw, ah);
                ds.readback_pending = true;
                ds.last_request_frame = ds.frame_counter;
            }
            ds.frame_counter += 1;
        }

        // Always run the upsample pass — if no inference has arrived
        // yet, the staging texture is all-zero and output is black.
        let pipeline = self.upsample_pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/depth_estimate_midas_upsample.wgsl"),
                "cs_main",
                "node.depth_map.upsample",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let Some(depth_state) = self.depth_state.as_ref() else {
            return;
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture: &depth_state.depth_texture,
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
            "node.depth_map",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn depth_estimate_midas_declares_one_input_and_one_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(DepthEstimateMidas::TYPE_ID, "node.depth_map");
        assert_eq!(DepthEstimateMidas::INPUTS.len(), 1);
        assert_eq!(DepthEstimateMidas::INPUTS[0].name, "in");
        assert_eq!(DepthEstimateMidas::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(DepthEstimateMidas::OUTPUTS.len(), 1);
        assert_eq!(DepthEstimateMidas::OUTPUTS[0].name, "out");
        assert_eq!(DepthEstimateMidas::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn depth_estimate_midas_has_analysis_and_interval_params() {
        let names: Vec<&str> = DepthEstimateMidas::PARAMS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(names, vec!["analysis_max_dim", "update_interval"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = DepthEstimateMidas::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.depth_map");
    }
}
