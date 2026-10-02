//! The GPU storage a liquid domain provides for its bodies: the body rows,
//! the source and drain region rows, the shapes and the half-precision
//! distance atlas (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` D2). Shapes and the
//! atlas are rebuilt into fresh buffers, so a buffer the GPU may still read is
//! never written; body and region rows are written in encoder order each frame.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::liquid::bodies::{LiquidBodies, LiquidBody, LiquidShape};

const UPLOAD_SHADER: &str = include_str!("../primitives/shaders/liquid_body_upload.wgsl");
/// 16-byte groups one inline upload carries (setBytes stays under 4 KB).
const UPLOAD_GROUPS: usize = 254;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UploadParams {
    start: u32,
    count: u32,
    _pad0: u32,
    _pad1: u32,
    words: [[u32; 4]; UPLOAD_GROUPS],
}

const _: () = assert!(std::mem::size_of::<UploadParams>() < 4096);

struct Stored {
    bodies: GpuBuffer,
    regions: GpuBuffer,
    shapes: GpuBuffer,
    atlas: GpuBuffer,
    version: u64,
}

/// A domain's provided `bodies`, `regions`, `shapes` and `atlas` outputs.
#[derive(Default)]
pub struct LiquidBodyBuffers {
    stored: Option<Stored>,
    upload: Option<GpuComputePipeline>,
}

impl LiquidBodyBuffers {
    pub fn bodies(&self) -> Option<&GpuBuffer> {
        self.stored.as_ref().map(|stored| &stored.bodies)
    }

    pub fn regions(&self) -> Option<&GpuBuffer> {
        self.stored.as_ref().map(|stored| &stored.regions)
    }

    pub fn shapes(&self) -> Option<&GpuBuffer> {
        self.stored.as_ref().map(|stored| &stored.shapes)
    }

    pub fn atlas(&self) -> Option<&GpuBuffer> {
        self.stored.as_ref().map(|stored| &stored.atlas)
    }

    /// The provided buffer behind a domain's `bodies`, `regions`, `shapes`
    /// or `atlas` output.
    pub fn output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "bodies" => self.bodies(),
            "regions" => self.regions(),
            "shapes" => self.shapes(),
            "atlas" => self.atlas(),
            _ => None,
        }
    }

    /// Keep the buffers current with `bodies`: a new shape version rebuilds
    /// shapes and atlas, and when `rows_fresh` this frame's body and region
    /// rows are written in encoder order. `label` names the dispatches for
    /// the profiler.
    pub fn upload(&mut self, gpu: &mut GpuEncoder<'_>, bodies: &LiquidBodies, rows_fresh: bool, label: &'static str) {
        let row_bytes = std::mem::size_of_val(bodies.last_rows());
        let region_bytes = std::mem::size_of_val(bodies.last_region_rows());
        let needs_bodies = self.stored.as_ref().is_none_or(|stored| stored.bodies.size < row_bytes as u64);
        let needs_regions = self.stored.as_ref().is_none_or(|stored| stored.regions.size < region_bytes as u64);
        let version = bodies.version;
        if self.stored.as_ref().is_none_or(|stored| stored.version != version) || needs_bodies || needs_regions {
            let fresh = |bytes: &[u8], least: usize| {
                let buffer = gpu.device.create_buffer_shared(bytes.len().max(least) as u64);
                // SAFETY: new shared buffer, not yet visible to the GPU.
                unsafe { buffer.write(0, bytes) };
                buffer
            };
            let row = std::mem::size_of::<LiquidBody>();
            let (old_bodies, old_regions) = match self.stored.take() {
                Some(stored) => (Some(stored.bodies), Some(stored.regions)),
                None => (None, None),
            };
            let keep = |old: Option<GpuBuffer>, needs: bool, bytes: usize| match old {
                Some(buffer) if !needs => buffer,
                _ => gpu.device.create_buffer_shared(bytes.max(row) as u64),
            };
            self.stored = Some(Stored {
                bodies: keep(old_bodies, needs_bodies, row_bytes),
                regions: keep(old_regions, needs_regions, region_bytes),
                shapes: fresh(bytemuck::cast_slice(bodies.shapes()), std::mem::size_of::<LiquidShape>()),
                atlas: fresh(bytemuck::cast_slice(bodies.atlas()), 4),
                version,
            });
        }
        if !rows_fresh {
            return;
        }
        let pipeline = self
            .upload
            .get_or_insert_with(|| gpu.device.create_compute_pipeline(UPLOAD_SHADER, "cs_main", label));
        let stored = self.stored.as_ref().expect("allocated above");
        for (target, rows) in [(&stored.bodies, bodies.last_rows()), (&stored.regions, bodies.last_region_rows())] {
            let groups: &[[u32; 4]] = bytemuck::cast_slice(rows);
            for (chunk_index, chunk) in groups.chunks(UPLOAD_GROUPS).enumerate() {
                let mut params: UploadParams = bytemuck::Zeroable::zeroed();
                let UploadParams { start, count, words, .. } = &mut params;
                *start = (chunk_index * UPLOAD_GROUPS) as u32;
                *count = chunk.len() as u32;
                words[..chunk.len()].copy_from_slice(chunk);
                gpu.native_enc.dispatch_compute(
                    pipeline,
                    &[
                        GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                        GpuBinding::Buffer { binding: 1, buffer: target, offset: 0 },
                    ],
                    [(chunk.len() as u32).div_ceil(64), 1, 1],
                    label,
                );
            }
        }
    }
}
