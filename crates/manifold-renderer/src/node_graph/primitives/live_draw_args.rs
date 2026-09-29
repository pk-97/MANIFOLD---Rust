//! Indirect draw arguments for scene objects whose mesh publishes a live
//! extent (GPU_FLUID_SURFACE_DESIGN.md P6b): one four-word block per object,
//! written on the GPU each frame from the extent's count, so every raster pass
//! draws only the live triangles.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::live_extent::LiveExtent;

/// Bytes of one object's draw arguments.
pub(super) const ARGS_BYTES: u64 = 16;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LiveArgs {
    word: u32,
    per_item: u32,
    capacity: u32,
    instances: u32,
    slot: u32,
    _pad: [u32; 3],
}

#[derive(Default)]
pub(super) struct LiveDrawArgs {
    pipeline: Option<GpuComputePipeline>,
    buffer: Option<GpuBuffer>,
}

impl LiveDrawArgs {
    /// The arguments buffer, holding at least `objects` blocks.
    pub(super) fn prepare(&mut self, device: &GpuDevice, objects: usize) -> GpuBuffer {
        if self.pipeline.is_none() {
            self.pipeline = Some(device.create_compute_pipeline(
                include_str!("shaders/live_draw_args.wgsl"),
                "write_draw_args",
                "node.render_scene live draw args",
            ));
        }
        let bytes = objects.max(1) as u64 * ARGS_BYTES;
        if self.buffer.as_ref().is_none_or(|buffer| buffer.size < bytes) {
            self.buffer = Some(device.create_buffer_shared(bytes));
        }
        self.buffer.clone().expect("arguments buffer allocated")
    }

    /// Encode block `slot`: the extent's live vertices as whole triangles,
    /// clamped to `vertices`' capacity, drawn `instances` times.
    pub(super) fn write(
        &self,
        encoder: &mut GpuEncoder,
        slot: usize,
        extent: &LiveExtent,
        vertices: &GpuBuffer,
        instances: u32,
    ) {
        let capacity = (vertices.size / std::mem::size_of::<MeshVertex>() as u64).min(u64::from(u32::MAX)) as u32;
        let params = LiveArgs {
            word: (extent.offset / 4) as u32,
            per_item: extent.per_item,
            capacity,
            instances,
            slot: slot as u32,
            _pad: [0; 3],
        };
        encoder.dispatch_compute(
            self.pipeline.as_ref().expect("prepare before write"),
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                GpuBinding::Buffer { binding: 1, buffer: &extent.counts, offset: 0 },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: self.buffer.as_ref().expect("prepare before write"),
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "node.render_scene live draw args",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests {
    use super::*;

    /// Whole live triangles, clamped to the vertex buffer's capacity, in the
    /// object's own four-word block.
    #[test]
    fn live_draw_args_are_whole_live_triangles_within_capacity() {
        let device = crate::test_device();
        let vertices = device.create_buffer_shared(18 * std::mem::size_of::<MeshVertex>() as u64);
        let mut writer = LiveDrawArgs::default();
        for (triangles, expected) in [(4u32, 12u32), (7, 18)] {
            let counts = device.create_buffer_shared(8);
            // SAFETY: fresh shared buffer, no GPU work in flight.
            unsafe { counts.write(0, bytemuck::cast_slice(&[0u32, triangles])) };
            let args = writer.prepare(&device, 2);
            let mut encoder = device.create_encoder("live draw args test");
            let extent = LiveExtent { counts, offset: 4, per_item: 3, bound: 18 };
            writer.write(&mut encoder, 1, &extent, &vertices, 5);
            encoder.commit_and_wait_completed();
            let ptr = args.mapped_ptr().expect("shared arguments buffer");
            let words = unsafe { std::slice::from_raw_parts(ptr as *const u32, 8) };
            assert_eq!(&words[4..], &[expected, 5, 0, 0], "{triangles} triangles in an 18-vertex buffer");
        }
    }
}
