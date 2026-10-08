//! Indirect draw arguments for scene objects, one aligned eight-word block
//! per object, written on the GPU each frame: from the mesh's live extent
//! (GPU_FLUID_SURFACE_DESIGN.md P6b), so every raster pass draws only the
//! live triangles, and from the instance array, so an instanced draw stops at
//! its last instance that is not an all-zero hole (`live_instances.wgsl`).

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use manifold_node_engine::scene::live_extent::LiveExtent;

/// Bytes of one object's draw arguments.
pub(super) const ARGS_BYTES: u64 = 32;

/// The most trim workgroups one dispatch takes; each strides past the grid.
const TRIM_GROUPS: u32 = 1024;

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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TrimArgs {
    slot: u32,
    count: u32,
    vertices: u32,
    _pad: u32,
}

#[derive(Default)]
pub(super) struct LiveDrawArgs {
    pipeline: Option<GpuComputePipeline>,
    fixed: Option<GpuComputePipeline>,
    trim: Option<GpuComputePipeline>,
    buffer: Option<GpuBuffer>,
}

impl LiveDrawArgs {
    /// The arguments buffer, holding at least `objects` blocks.
    pub(super) fn prepare(&mut self, device: &GpuDevice, objects: usize) -> GpuBuffer {
        if self.pipeline.is_none() {
            self.pipeline = Some(device.create_compute_pipeline(
                include_str!("../shaders/live_draw_args.wgsl"),
                "write_draw_args",
                "node.render_scene live draw args",
            ));
            self.fixed = Some(device.create_compute_pipeline(
                include_str!("../shaders/live_instances.wgsl"),
                "fixed_args",
                "node.render_scene fixed draw args",
            ));
            self.trim = Some(device.create_compute_pipeline(
                include_str!("../shaders/live_instances.wgsl"),
                "trim_instances",
                "node.render_scene live instances",
            ));
        }
        let bytes = objects.max(1) as u64 * ARGS_BYTES;
        if self.buffer.as_ref().is_none_or(|buffer| buffer.size < bytes) {
            self.buffer = Some(device.create_buffer_shared(bytes));
        }
        self.buffer.clone().expect("arguments buffer allocated")
    }

    /// Encode block `slot`: the extent's live vertices as whole triangles,
    /// clamped to the vertex or index buffer capacity, drawn `instances` times.
    pub(super) fn write(
        &self,
        encoder: &mut GpuEncoder,
        slot: usize,
        extent: &LiveExtent,
        capacity: u32,
        instances: u32,
    ) {
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

    /// Encode block `slot` for a mesh without a live extent: `vertices`
    /// (indices when indexed) and zero instances, for [`Self::trim`].
    pub(super) fn write_fixed(&self, encoder: &mut GpuEncoder, slot: usize, vertices: u32) {
        let params = TrimArgs { slot: slot as u32, count: 0, vertices, _pad: 0 };
        encoder.dispatch_compute(
            self.fixed.as_ref().expect("prepare before write"),
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                GpuBinding::Buffer { binding: 2, buffer: self.buffer.as_ref().expect("prepare before write"), offset: 0 },
            ],
            [1, 1, 1],
            "node.render_scene fixed draw args",
        );
    }

    /// Raise block `slot`'s instance word, written as zero and behind a
    /// buffer barrier, to one past the last of the first `count` instances
    /// that is not all zero.
    pub(super) fn trim(&self, encoder: &mut GpuEncoder, slot: usize, instances: &GpuBuffer, count: u32) {
        let params = TrimArgs { slot: slot as u32, count, vertices: 0, _pad: 0 };
        encoder.dispatch_compute(
            self.trim.as_ref().expect("prepare before trim"),
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                GpuBinding::Buffer { binding: 1, buffer: instances, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: self.buffer.as_ref().expect("prepare before trim"), offset: 0 },
            ],
            [count.div_ceil(256).clamp(1, TRIM_GROUPS), 1, 1],
            "node.render_scene live instances",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests {
    use super::*;

    /// Whole live triangles, clamped to the vertex buffer's capacity, in the
    /// object's own aligned block.
    #[test]
    fn live_draw_args_are_whole_live_triangles_within_capacity() {
        let device = manifold_gpu::testkit::test_device();
        let mut writer = LiveDrawArgs::default();
        for (triangles, expected) in [(4u32, 12u32), (7, 18)] {
            let counts = device.create_buffer_shared(8);
            // SAFETY: fresh shared buffer, no GPU work in flight.
            unsafe { counts.write(0, bytemuck::cast_slice(&[0u32, triangles])) };
            let args = writer.prepare(&device, 2);
            let mut encoder = device.create_encoder("live draw args test");
            let extent = LiveExtent { counts, offset: 4, per_item: 3, bound: 18 };
            writer.write(&mut encoder, 1, &extent, 18, 5);
            encoder.commit_and_wait_completed();
            let ptr = args.mapped_ptr().expect("shared arguments buffer");
            let words = unsafe { std::slice::from_raw_parts(ptr as *const u32, 16) };
            assert_eq!(&words[8..13], &[expected, 5, 0, 0, 0], "{triangles} triangles in an 18-vertex buffer");
        }
    }

    /// One past the last instance that is not all zero, within the count;
    /// holes before it still count. Over 1024 groups' worth, the stride
    /// reaches the end.
    #[test]
    fn trimmed_instances_end_after_the_last_non_hole() {
        let device = manifold_gpu::testkit::test_device();
        let mut writer = LiveDrawArgs::default();
        let live = [1.0f32, 2.0, 3.0, 0.5, 0.0, 0.0, 0.0, 0.0];
        let hole = [0.0f32; 8];
        let mut far = vec![hole; 300_000];
        far[299_998] = live;
        let mut sign_only = hole;
        sign_only[7] = -1.0;
        let cases: [(Vec<[f32; 8]>, u32, u32); 6] = [
            (vec![live, hole, live, hole, hole], 5, 3),
            (vec![hole, hole, hole], 3, 0),
            (vec![live, hole, live], 2, 1),
            (vec![hole, sign_only], 2, 2),
            (vec![live; 1000], 1000, 1000),
            (far, 300_000, 299_999),
        ];
        for (instances, count, expected) in cases {
            let buffer = device.create_buffer_shared((instances.len() * 32) as u64);
            // SAFETY: fresh shared buffer, no GPU work in flight.
            unsafe { buffer.write(0, bytemuck::cast_slice(&instances)) };
            let args = writer.prepare(&device, 2);
            let mut encoder = device.create_encoder("live instances test");
            writer.write_fixed(&mut encoder, 1, 36);
            encoder.compute_memory_barrier_buffers();
            writer.trim(&mut encoder, 1, &buffer, count);
            encoder.commit_and_wait_completed();
            let ptr = args.mapped_ptr().expect("shared arguments buffer");
            let words = unsafe { std::slice::from_raw_parts(ptr as *const u32, 16) };
            assert_eq!(&words[8..13], &[36, expected, 0, 0, 0], "{} instances, count {count}", instances.len());
        }
    }
}
