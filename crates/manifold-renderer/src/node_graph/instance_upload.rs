//! Immutable CPU-to-GPU uploads for bounded `InstanceTransform` snapshots.
//!
//! The encoder owns the inline bytes until command-buffer completion. A
//! publisher can therefore replace its source slice immediately after
//! `upload` returns without racing an in-flight Metal command buffer.

use crate::generators::mesh_common::InstanceTransform;
use crate::gpu_encoder::GpuEncoder;
use bytemuck::Zeroable;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

const INSTANCE_UPLOAD_WGSL: &str = include_str!("primitives/shaders/physics_instance_upload.wgsl");
const INSTANCES_PER_UPLOAD: usize = 64;
const WORKGROUP_SIZE: u32 = 64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UploadParams {
    start: u32,
    count: u32,
    source_count: u32,
    _pad0: u32,
    values: [[f32; 4]; INSTANCES_PER_UPLOAD * 2],
}

const _: () = assert!(std::mem::size_of::<UploadParams>() < 4096);

/// Version- and destination-aware upload state for immutable instance data.
#[derive(Default)]
pub(crate) struct InstanceSnapshotUpload {
    pipeline: Option<GpuComputePipeline>,
    last_version: Option<u64>,
    last_destination: Option<usize>,
    last_len: usize,
}

impl InstanceSnapshotUpload {
    /// Populate the shared compute-pipeline cache before live rendering.
    pub(crate) fn prewarm(device: &GpuDevice) {
        device.create_compute_pipeline(
            INSTANCE_UPLOAD_WGSL,
            "cs_main",
            "node.physics_world.instances",
        );
    }

    /// Encode an immutable instance snapshot into `dst`.
    pub(crate) fn upload(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        dst: &GpuBuffer,
        instances: &[InstanceTransform],
        version: u64,
        retained: bool,
    ) -> Result<bool, &'static str> {
        let capacity = (dst.size / std::mem::size_of::<InstanceTransform>() as u64) as usize;
        if instances.len() > capacity {
            return Err("instance snapshot upload exceeds destination capacity");
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
                INSTANCE_UPLOAD_WGSL,
                "cs_main",
                "node.physics_world.instances",
            )
        });
        for (chunk_index, chunk) in instances.chunks(INSTANCES_PER_UPLOAD).enumerate() {
            Self::dispatch(
                gpu,
                pipeline,
                dst,
                (chunk_index * INSTANCES_PER_UPLOAD) as u32,
                chunk,
                chunk.len() as u32,
            );
        }

        let destination_changed = self.last_destination != Some(destination);
        let clear_count = if destination_changed || !retained {
            capacity - instances.len()
        } else if instances.len() < self.last_len {
            self.last_len - instances.len()
        } else {
            0
        };
        if clear_count != 0 {
            Self::dispatch(
                gpu,
                pipeline,
                dst,
                instances.len() as u32,
                &[],
                clear_count as u32,
            );
        }
        self.last_version = Some(version);
        self.last_destination = Some(destination);
        self.last_len = instances.len();
        Ok(true)
    }

    fn dispatch(
        gpu: &mut GpuEncoder<'_>,
        pipeline: &GpuComputePipeline,
        dst: &GpuBuffer,
        start: u32,
        source: &[InstanceTransform],
        count: u32,
    ) {
        debug_assert!(source.len() <= INSTANCES_PER_UPLOAD);
        let mut params = UploadParams::zeroed();
        params.start = start;
        params.count = count;
        params.source_count = start.saturating_add(source.len() as u32);
        for (index, instance) in source.iter().enumerate() {
            params.values[index * 2] = instance.pos_scale;
            params.values[index * 2 + 1] = instance.rot_pad;
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
            "node.physics_world.instances",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::gpu_encoder::GpuEncoder;

    fn instance(index: usize) -> InstanceTransform {
        InstanceTransform {
            pos_scale: [index as f32, 2.0, 3.0, 1.0],
            rot_pad: [0.1, 0.2, 0.3, index as f32],
        }
    }

    fn read_instances(buffer: &GpuBuffer, count: usize) -> Vec<InstanceTransform> {
        let ptr = buffer.mapped_ptr().expect("shared output buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const InstanceTransform, count).to_vec() }
    }

    fn assert_instances_equal(actual: &[InstanceTransform], expected: &[InstanceTransform]) {
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected) {
            assert_eq!(bytemuck::bytes_of(actual), bytemuck::bytes_of(expected));
        }
    }

    fn encode(
        device: &GpuDevice,
        upload: &mut InstanceSnapshotUpload,
        dst: &GpuBuffer,
        source: &[InstanceTransform],
        version: u64,
        retained: bool,
    ) -> Result<bool, &'static str> {
        let mut native = device.create_encoder("instance-snapshot-upload-test");
        let result = {
            let mut gpu = GpuEncoder::new(&mut native, device);
            upload.upload(&mut gpu, dst, source, version, retained)
        };
        native.commit_and_wait_completed();
        result
    }

    #[test]
    fn version_skip_and_destination_change() {
        let device = crate::test_device();
        let first =
            device.create_buffer_shared(8 * std::mem::size_of::<InstanceTransform>() as u64);
        let second =
            device.create_buffer_shared(8 * std::mem::size_of::<InstanceTransform>() as u64);
        let source: Vec<_> = (0..3).map(instance).collect();
        let mut upload = InstanceSnapshotUpload::default();
        assert_eq!(
            encode(&device, &mut upload, &first, &source, 1, true),
            Ok(true)
        );
        assert_eq!(
            encode(&device, &mut upload, &first, &source, 1, true),
            Ok(false)
        );
        assert_eq!(
            encode(&device, &mut upload, &second, &source, 1, true),
            Ok(true)
        );
        assert_instances_equal(&read_instances(&second, 3), &source);
    }

    #[test]
    fn shrink_and_empty_clear_stale_tail() {
        let device = crate::test_device();
        let dst = device.create_buffer_shared(12 * std::mem::size_of::<InstanceTransform>() as u64);
        let source: Vec<_> = (0..9).map(instance).collect();
        let short: Vec<_> = (0..3).map(|index| instance(index + 20)).collect();
        let mut upload = InstanceSnapshotUpload::default();
        assert_eq!(
            encode(&device, &mut upload, &dst, &source, 1, true),
            Ok(true)
        );
        assert_eq!(
            encode(&device, &mut upload, &dst, &short, 2, true),
            Ok(true)
        );
        assert_instances_equal(&read_instances(&dst, 3), &short);
        assert!(
            read_instances(&dst, 12)[3..]
                .iter()
                .all(|value| bytemuck::bytes_of(value).iter().all(|byte| *byte == 0))
        );
        assert_eq!(encode(&device, &mut upload, &dst, &[], 3, true), Ok(true));
        assert!(
            read_instances(&dst, 12)
                .iter()
                .all(|value| bytemuck::bytes_of(value).iter().all(|byte| *byte == 0))
        );
    }

    #[test]
    fn overflow_rejects_before_touching_destination() {
        let device = crate::test_device();
        let dst = device.create_buffer_shared(3 * std::mem::size_of::<InstanceTransform>() as u64);
        unsafe {
            std::ptr::write_bytes(
                dst.mapped_ptr().expect("shared destination buffer"),
                0xa5,
                dst.size as usize,
            );
        }
        let source: Vec<_> = (0..4).map(instance).collect();
        let mut upload = InstanceSnapshotUpload::default();
        assert_eq!(
            encode(&device, &mut upload, &dst, &source, 1, true),
            Err("instance snapshot upload exceeds destination capacity")
        );
        let bytes = unsafe {
            std::slice::from_raw_parts(
                dst.mapped_ptr().expect("shared destination buffer"),
                dst.size as usize,
            )
        };
        assert!(bytes.iter().all(|byte| *byte == 0xa5));
    }

    #[test]
    fn sequential_queued_snapshots_keep_inline_data() {
        let device = crate::test_device();
        let first =
            device.create_buffer_shared(3 * std::mem::size_of::<InstanceTransform>() as u64);
        let second =
            device.create_buffer_shared(3 * std::mem::size_of::<InstanceTransform>() as u64);
        let first_source: Vec<_> = (0..3).map(instance).collect();
        let second_source: Vec<_> = (100..103).map(instance).collect();
        let mut first_upload = InstanceSnapshotUpload::default();
        let mut second_upload = InstanceSnapshotUpload::default();
        let mut native = device.create_encoder("instance-snapshot-sequential-test");
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
        assert_instances_equal(&read_instances(&first, 3), &first_source);
        assert_instances_equal(&read_instances(&second, 3), &second_source);
    }
}
