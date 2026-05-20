//! `node.blob_detect_ffi` — sparse blob detection via the
//! `manifold_native::BlobDetector` FFI plugin, wrapped as a
//! primitive that emits an `Array<Blob>`.
//!
//! Output: `Array<Blob>` of `max_capacity` entries. The first
//! N entries (N = detected count, capped at `MAX_BLOB_CAP = 32`)
//! hold valid blobs in normalized 0..1 image space; remaining
//! entries are zeroed.
//!
//! Pair with a future `node.blob_overlay_render` (or any custom
//! consumer that iterates `Array<Blob>`) to draw the boxes.
//!
//! Same readback / background-worker pattern as the depth and
//! flow primitives: the input frame is read back to a CPU buffer
//! at analysis resolution, blob detection runs on a worker thread,
//! the result is uploaded as a fixed-size byte buffer and pushed
//! into the GPU storage buffer by a tiny compute pass.

#![allow(private_interfaces)]

use manifold_gpu::{GpuBinding, GpuComputePipeline};
use manifold_native::blob_detector::BlobDetector;

use crate::background_worker::BackgroundWorker;
use crate::generators::mesh_common::Blob;
use crate::gpu_readback::ReadbackRequest;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Maximum number of blobs the FFI plugin is configured to track AND
/// the WGSL uniform's fixed array size. Keep these in sync with the
/// `MAX_BLOB_CAP` constant in `shaders/blob_detect_ffi_upload.wgsl`.
const MAX_BLOB_CAP: usize = 32;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UploadUniforms {
    count: u32,
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
}

struct BlobRequest {
    pixel_data: Vec<u8>,
    width: i32,
    height: i32,
    threshold: f32,
    sensitivity: f32,
}

struct BlobResponse {
    /// Up to `MAX_BLOB_CAP` blobs; the worker also returns the
    /// inferenced count via the slice length.
    blobs: Vec<Blob>,
}

struct BlobState {
    analysis_width: u32,
    analysis_height: u32,
    readback: ReadbackRequest,
    readback_pending: bool,
    blobs_dirty: bool,
    /// Live blob list (count = blobs.len()).
    blobs: Vec<Blob>,
    last_request_frame: i64,
    frame_counter: i64,
}

crate::primitive! {
    name: BlobDetectFfi,
    type_id: "node.blob_detect_ffi",
    purpose: "Sparse blob detection (bright-region tracking) via the manifold_native BlobDetector FFI plugin. Input: any Texture2D. Output: Array<Blob> (16-byte items: x, y, width, height in normalized 0..1 image space). First N entries are valid blobs (N = detected count, capped at 32); remaining entries are zeroed. Pair with downstream blob-overlay render primitives to draw the boxes, or wire to any consumer that iterates Array<Blob>.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        blobs: Array(Blob),
    },
    params: [
        ParamDef {
            name: "max_capacity",
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Int(32),
            range: Some((1.0, 32.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "threshold",
            label: "Threshold",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "sensitivity",
            label: "Sensitivity",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "analysis_max_dim",
            label: "Analysis Max Dim",
            ty: ParamType::Int,
            default: ParamValue::Int(360),
            range: Some((64.0, 1024.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "update_interval",
            label: "Update Interval (frames)",
            ty: ParamType::Int,
            default: ParamValue::Int(2),
            range: Some((1.0, 30.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "max_capacity is read by the chain build at allocation time (set once when authoring the preset). threshold sets the brightness cutoff in 0..1; sensitivity controls how aggressively bright regions merge into one blob. Until the first inference completes, the output buffer is all zeros — downstream consumers should skip zero-size entries.",
    examples: [],
    picker: { label: "Blob Detect (FFI)", category: Atom },
    extra_fields: {
        upload_pipeline: Option<GpuComputePipeline> = None,
        blob_worker: Option<BackgroundWorker<BlobRequest, BlobResponse>> = None,
        blob_worker_tried: bool = false,
        blob_state: Option<BlobState> = None,
    },
}

impl BlobDetectFfi {
    fn ensure_blob_worker(&mut self) {
        if self.blob_worker.is_some() || self.blob_worker_tried {
            return;
        }
        self.blob_worker_tried = true;
        self.blob_worker = BackgroundWorker::try_new(|| {
            let detector =
                manifold_native::ffi::blob_ffi::FfiBlobDetector::new(MAX_BLOB_CAP as i32)?;
            log::info!(
                "[node.blob_detect_ffi] Blob detector worker spawned (max {} blobs)",
                MAX_BLOB_CAP
            );
            Some(move |req: BlobRequest| -> BlobResponse {
                let mut raw = vec![0f32; MAX_BLOB_CAP * 4];
                let count = detector.process(
                    &req.pixel_data,
                    req.width,
                    req.height,
                    req.threshold,
                    req.sensitivity,
                    &mut raw,
                );
                let n = (count.max(0) as usize).min(MAX_BLOB_CAP);
                let mut blobs = Vec::with_capacity(n);
                for i in 0..n {
                    blobs.push(Blob {
                        x: raw[i * 4],
                        y: raw[i * 4 + 1],
                        width: raw[i * 4 + 2],
                        height: raw[i * 4 + 3],
                    });
                }
                BlobResponse { blobs }
            })
        });
        if self.blob_worker.is_none() {
            log::warn!(
                "[node.blob_detect_ffi] Native blob detector unavailable — output will be all zeros"
            );
        }
    }

    fn ensure_blob_state(&mut self, width: u32, height: u32, analysis_max_dim: u32) {
        let max_dim = width.max(height);
        let scale = if max_dim == 0 {
            1.0
        } else {
            (analysis_max_dim as f32 / max_dim as f32).min(1.0)
        };
        let aw = ((width as f32 * scale).round() as u32).max(64);
        let ah = ((height as f32 * scale).round() as u32).max(36);

        let needs_rebuild = match &self.blob_state {
            Some(bs) => bs.analysis_width != aw || bs.analysis_height != ah,
            None => true,
        };
        if !needs_rebuild {
            return;
        }
        self.blob_state = Some(BlobState {
            analysis_width: aw,
            analysis_height: ah,
            readback: ReadbackRequest::new(),
            readback_pending: false,
            blobs_dirty: false,
            blobs: Vec::new(),
            last_request_frame: -1024,
            frame_counter: 0,
        });
    }
}

impl Primitive for BlobDetectFfi {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let threshold = match ctx.params.get("threshold") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        };
        let sensitivity = match ctx.params.get("sensitivity") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        };
        let analysis_max_dim = match ctx.params.get("analysis_max_dim") {
            Some(ParamValue::Int(i)) => (*i).max(64) as u32,
            _ => 360,
        };
        let update_interval = match ctx.params.get("update_interval") {
            Some(ParamValue::Int(i)) => (*i).max(1) as i64,
            _ => 2,
        };

        let Some(source) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("blobs") else {
            return;
        };
        let blob_size = std::mem::size_of::<Blob>() as u64;
        let capacity = (out_buf.size / blob_size) as u32;

        let gpu = ctx.gpu_encoder();
        self.ensure_blob_worker();
        self.ensure_blob_state(source.width, source.height, analysis_max_dim);

        if let (Some(bs), Some(bw)) = (self.blob_state.as_mut(), self.blob_worker.as_mut()) {
            // Poll readback → submit to worker.
            if bs.readback_pending
                && let Some(pixels) = bs.readback.try_read()
            {
                bs.readback_pending = false;
                bw.submit(BlobRequest {
                    pixel_data: pixels,
                    width: bs.analysis_width as i32,
                    height: bs.analysis_height as i32,
                    threshold,
                    sensitivity,
                });
            }

            // Poll worker result → mark dirty.
            if let Some(response) = bw.try_recv() {
                bs.blobs = response.blobs;
                bs.blobs_dirty = true;
            }

            // Submit fresh readback every `update_interval` frames.
            let elapsed = bs.frame_counter - bs.last_request_frame;
            if elapsed >= update_interval && !bs.readback.is_pending() {
                let aw = bs.analysis_width;
                let ah = bs.analysis_height;
                let staging = gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
                    width: aw,
                    height: ah,
                    depth: 1,
                    format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                    dimension: manifold_gpu::GpuTextureDimension::D2,
                    usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
                    label: "node.blob_detect_ffi.staging",
                    mip_levels: 1,
                });
                gpu.copy_texture_to_texture(source, &staging, aw, ah);
                bs.readback.submit(gpu, &staging, aw, ah);
                bs.readback_pending = true;
                bs.last_request_frame = bs.frame_counter;
            }
            bs.frame_counter += 1;
        }

        // Build the fixed-cap upload buffer every frame (cheap, ~512 bytes)
        // so a re-dispatch always sees the latest detection. If the worker
        // is unavailable, this stays all-zero and the output gets zeroed.
        let mut src_blobs = [Blob::default(); MAX_BLOB_CAP];
        let mut count: u32 = 0;
        if let Some(bs) = self.blob_state.as_ref() {
            let n = bs.blobs.len().min(MAX_BLOB_CAP);
            for i in 0..n {
                src_blobs[i] = bs.blobs[i];
            }
            count = n as u32;
        }
        if let Some(bs) = self.blob_state.as_mut() {
            bs.blobs_dirty = false;
        }

        let uniforms = UploadUniforms {
            count,
            capacity,
            _pad0: 0,
            _pad1: 0,
        };

        let pipeline = self.upload_pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/blob_detect_ffi_upload.wgsl"),
                "cs_main",
                "node.blob_detect_ffi.upload",
            )
        });

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Bytes {
                    binding: 1,
                    data: bytemuck::cast_slice(&src_blobs),
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(64), 1, 1],
            "node.blob_detect_ffi",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn blob_detect_ffi_declares_texture_in_and_array_blob_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let blob_layout = ArrayType {
            item_size: std::mem::size_of::<Blob>() as u32,
            item_align: std::mem::align_of::<Blob>() as u32,
        };

        assert_eq!(BlobDetectFfi::TYPE_ID, "node.blob_detect_ffi");
        assert_eq!(BlobDetectFfi::INPUTS.len(), 1);
        assert_eq!(BlobDetectFfi::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(BlobDetectFfi::OUTPUTS.len(), 1);
        assert_eq!(BlobDetectFfi::OUTPUTS[0].name, "blobs");
        assert_eq!(BlobDetectFfi::OUTPUTS[0].ty, PortType::Array(blob_layout));
    }

    #[test]
    fn blob_detect_ffi_has_full_param_surface() {
        let names: Vec<&str> = BlobDetectFfi::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec![
                "max_capacity",
                "threshold",
                "sensitivity",
                "analysis_max_dim",
                "update_interval"
            ]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = BlobDetectFfi::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.blob_detect_ffi");
    }

    #[test]
    fn blob_struct_is_16_bytes_for_array_wire() {
        // Layout invariant — if this changes, the WGSL `Blob` struct
        // in blob_detect_ffi_upload.wgsl must match.
        assert_eq!(std::mem::size_of::<Blob>(), 16);
        assert_eq!(std::mem::align_of::<Blob>(), 4);
    }
}
