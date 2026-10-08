//! CPU-to-GPU bridge for immutable fluid mesh snapshots.
//!
//! The fluid worker owns the source `Vec<MeshVertex>` and publishes a versioned
//! immutable snapshot.  Uploads therefore use inline Metal bytes: the command
//! encoder snapshots each chunk, so a later worker publication cannot race a
//! buffer that is still in flight.

use crate::mesh::MeshVertex;
use crate::gpu::gpu_encoder::GpuEncoder;
use bytemuck::Zeroable;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

const FLUID_MESH_UPLOAD_WGSL: &str = include_str!("primitives/shaders/fluid_mesh_upload.wgsl");
const MESH_VERTEX_SIZE: usize = std::mem::size_of::<MeshVertex>();
const MAX_VERTICES_PER_UPLOAD: usize = 50;
const WORKGROUP_SIZE: u32 = 64;
const VEC4S_PER_VERTEX: usize = MESH_VERTEX_SIZE / std::mem::size_of::<[f32; 4]>();

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UploadParams {
    start: u32,
    count: u32,
    source_count: u32,
    _pad0: u32,
    values: [[f32; 4]; MAX_VERTICES_PER_UPLOAD * VEC4S_PER_VERTEX],
}

const _: () = assert!(std::mem::size_of::<UploadParams>() <= 4096);
const _: () = assert!(std::mem::size_of::<UploadParams>() == 4016);

/// Version and destination gate for a CPU-origin fluid mesh upload.
#[derive(Default)]
pub struct FluidMeshUpload {
    pipeline: Option<GpuComputePipeline>,
    last_version: Option<u64>,
    last_destination: Option<usize>,
    last_len: usize,
}

impl FluidMeshUpload {
    /// Populate the device's shared compute-pipeline cache before live use.
    pub fn prewarm(device: &GpuDevice) {
        device.create_compute_pipeline(FLUID_MESH_UPLOAD_WGSL, "cs_main", "node.fluid_mesh_upload");
    }

    /// Encode an immutable CPU mesh snapshot into `dst`.
    ///
    /// The destination is a triangle-list `Array<MeshVertex>` storage buffer.
    /// Unused vertices are explicitly zeroed so a shorter snapshot cannot leave
    /// stale geometry visible to the draw consumer.
    pub fn upload(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        dst: &GpuBuffer,
        vertices: &[MeshVertex],
        version: u64,
        retained: bool,
    ) -> Result<bool, &'static str> {
        if !vertices.len().is_multiple_of(3) {
            return Err("fluid mesh upload source length must be a multiple of 3");
        }

        let capacity = (dst.size / MESH_VERTEX_SIZE as u64) as usize;
        if vertices.len() > capacity {
            return Err("fluid mesh upload exceeds destination capacity");
        }

        let destination = dst.identity_key();
        if retained
            && self.last_version == Some(version)
            && self.last_destination == Some(destination)
        {
            return Ok(false);
        }

        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                FLUID_MESH_UPLOAD_WGSL,
                "cs_main",
                "node.fluid_mesh_upload",
            )
        });

        for (chunk_index, chunk) in vertices.chunks(MAX_VERTICES_PER_UPLOAD).enumerate() {
            Self::dispatch(
                gpu,
                pipeline,
                dst,
                (chunk_index * MAX_VERTICES_PER_UPLOAD) as u32,
                chunk,
                chunk.len() as u32,
            );
        }

        let destination_changed = self.last_destination != Some(destination);
        let clear_count = if destination_changed || !retained {
            capacity - vertices.len()
        } else if vertices.len() < self.last_len {
            self.last_len - vertices.len()
        } else {
            0
        };
        if clear_count != 0 {
            Self::dispatch(
                gpu,
                pipeline,
                dst,
                vertices.len() as u32,
                &[],
                clear_count as u32,
            );
        }

        // Keep the gate state coherent only after every required dispatch has
        // been handed to the encoder.
        self.last_version = Some(version);
        self.last_destination = Some(destination);
        self.last_len = vertices.len();
        Ok(true)
    }

    fn dispatch(
        gpu: &mut GpuEncoder<'_>,
        pipeline: &GpuComputePipeline,
        dst: &GpuBuffer,
        start: u32,
        source: &[MeshVertex],
        count: u32,
    ) {
        debug_assert!(source.len() <= MAX_VERTICES_PER_UPLOAD);
        debug_assert!(source.is_empty() || source.len() == count as usize);
        let mut params = UploadParams::zeroed();
        params.start = start;
        params.count = count;
        params.source_count = source.len() as u32;
        if !source.is_empty() {
            bytemuck::cast_slice_mut::<[f32; 4], u8>(&mut params.values)
                .get_mut(..std::mem::size_of_val(source))
                .expect("fluid mesh upload inline payload is large enough")
                .copy_from_slice(bytemuck::cast_slice(source));
        }

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [count.div_ceil(WORKGROUP_SIZE), 1, 1],
            "node.fluid_mesh_upload",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;

    fn vertex(index: usize) -> MeshVertex {
        let value = index as f32;
        MeshVertex {
            position: [value, value + 0.25, value + 0.5],
            _pad0: 0.0,
            normal: [0.0, 1.0, value],
            _pad1: 0.0,
            uv: [value * 0.01, value * 0.02],
            _pad2: [0.0; 2],
            tangent: [1.0, 0.0, 0.0, 1.0],
            color: [value + 1.0, value + 2.0, value + 3.0, 1.0],
        }
    }

    fn assert_vertices_equal(actual: &[MeshVertex], expected: &[MeshVertex]) {
        assert_eq!(
            bytemuck::cast_slice::<_, u8>(actual),
            bytemuck::cast_slice::<_, u8>(expected)
        );
    }

    fn read_vertices(dst: &GpuBuffer, count: usize) -> Vec<MeshVertex> {
        let ptr = dst.mapped_ptr().expect("shared destination buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, count) }.to_vec()
    }

    fn read_vertex_range(dst: &GpuBuffer, start: usize, count: usize) -> Vec<MeshVertex> {
        let ptr = dst.mapped_ptr().expect("shared destination buffer");
        unsafe { std::slice::from_raw_parts((ptr as *const MeshVertex).add(start), count) }.to_vec()
    }

    fn encode(
        device: &GpuDevice,
        upload: &mut FluidMeshUpload,
        dst: &GpuBuffer,
        vertices: &[MeshVertex],
        version: u64,
        retained: bool,
    ) -> Result<bool, &'static str> {
        let mut native = device.create_encoder("fluid-mesh-upload-test");
        let result = {
            let mut gpu = GpuEncoder::new(&mut native, device);
            upload.upload(&mut gpu, dst, vertices, version, retained)
        };
        native.commit_and_wait_completed();
        result
    }

    #[test]
    fn uploads_more_than_two_inline_chunks_with_distinct_boundary_colors() {
        let device = manifold_gpu::testkit::test_device();
        let dst = device.create_buffer_shared((200 * MESH_VERTEX_SIZE) as u64);
        let source: Vec<_> = (0..189).map(vertex).collect();
        let mut upload = FluidMeshUpload::default();
        assert_ne!(source[49].color, source[50].color);

        assert_eq!(
            encode(&device, &mut upload, &dst, &source, 1, true),
            Ok(true)
        );
        assert_vertices_equal(&read_vertices(&dst, source.len()), &source);
        assert!(read_vertex_range(&dst, source.len(), 11)
            .iter()
            .all(|v| bytemuck::bytes_of(v).iter().all(|b| *b == 0)));
    }

    #[test]
    fn shrink_and_empty_snapshot_clear_the_old_tail() {
        let device = manifold_gpu::testkit::test_device();
        let dst = device.create_buffer_shared((12 * MESH_VERTEX_SIZE) as u64);
        let source: Vec<_> = (0..9).map(vertex).collect();
        let short: Vec<_> = (0..3).map(vertex).collect();
        let mut upload = FluidMeshUpload::default();

        assert_eq!(
            encode(&device, &mut upload, &dst, &source, 1, true),
            Ok(true)
        );
        assert_eq!(
            encode(&device, &mut upload, &dst, &short, 2, true),
            Ok(true)
        );
        assert!(read_vertices(&dst, 9)[3..]
            .iter()
            .all(|v| bytemuck::bytes_of(v).iter().all(|b| *b == 0)));
        assert_eq!(encode(&device, &mut upload, &dst, &[], 3, true), Ok(true));
        assert!(read_vertices(&dst, 12)
            .iter()
            .all(|v| bytemuck::bytes_of(v).iter().all(|b| *b == 0)));
    }

    #[test]
    fn same_version_on_a_new_destination_still_uploads() {
        let device = manifold_gpu::testkit::test_device();
        let first = device.create_buffer_shared((6 * MESH_VERTEX_SIZE) as u64);
        let second = device.create_buffer_shared((6 * MESH_VERTEX_SIZE) as u64);
        let source: Vec<_> = (0..3).map(vertex).collect();
        let mut upload = FluidMeshUpload::default();

        assert_eq!(
            encode(&device, &mut upload, &first, &source, 4, true),
            Ok(true)
        );
        assert_eq!(
            encode(&device, &mut upload, &second, &source, 4, true),
            Ok(true)
        );
        assert_vertices_equal(&read_vertices(&second, source.len()), &source);
    }

    #[test]
    fn non_retained_reencodes_and_retained_unchanged_skips() {
        let device = manifold_gpu::testkit::test_device();
        let dst = device.create_buffer_shared((6 * MESH_VERTEX_SIZE) as u64);
        let source: Vec<_> = (0..3).map(vertex).collect();
        let mut upload = FluidMeshUpload::default();

        assert_eq!(
            encode(&device, &mut upload, &dst, &source, 7, true),
            Ok(true)
        );
        assert_eq!(
            encode(&device, &mut upload, &dst, &source, 7, true),
            Ok(false)
        );
        assert_eq!(
            encode(&device, &mut upload, &dst, &source, 7, false),
            Ok(true)
        );
    }

    #[test]
    fn overflow_rejects_before_touching_destination() {
        let device = manifold_gpu::testkit::test_device();
        let dst = device.create_buffer_shared((3 * MESH_VERTEX_SIZE) as u64);
        let sentinel = 0xa5u8;
        unsafe {
            std::ptr::write_bytes(
                dst.mapped_ptr().expect("shared destination buffer"),
                sentinel,
                dst.size as usize,
            );
        }
        let source: Vec<_> = (0..6).map(vertex).collect();
        let mut upload = FluidMeshUpload::default();

        assert_eq!(
            encode(&device, &mut upload, &dst, &source, 1, true),
            Err("fluid mesh upload exceeds destination capacity")
        );
        let bytes = unsafe {
            std::slice::from_raw_parts(
                dst.mapped_ptr().expect("shared destination buffer"),
                dst.size as usize,
            )
        };
        assert!(bytes.iter().all(|byte| *byte == sentinel));
    }

    #[test]
    fn inline_snapshots_survive_two_uploads_before_commit() {
        let device = manifold_gpu::testkit::test_device();
        let first = device.create_buffer_shared((3 * MESH_VERTEX_SIZE) as u64);
        let second = device.create_buffer_shared((3 * MESH_VERTEX_SIZE) as u64);
        let first_source: Vec<_> = (0..3).map(vertex).collect();
        let second_source: Vec<_> = (100..103).map(vertex).collect();
        let mut first_upload = FluidMeshUpload::default();
        let mut second_upload = FluidMeshUpload::default();
        let mut native = device.create_encoder("fluid-mesh-upload-snapshot-test");
        {
            let mut gpu = GpuEncoder::new(&mut native, &device);
            assert_eq!(
                first_upload.upload(&mut gpu, &first, &first_source, 1, true),
                Ok(true)
            );
            assert_eq!(
                second_upload.upload(&mut gpu, &second, &second_source, 1, true),
                Ok(true)
            );
        }
        native.commit_and_wait_completed();

        assert_vertices_equal(&read_vertices(&first, 3), &first_source);
        assert_vertices_equal(&read_vertices(&second, 3), &second_source);
    }
}
