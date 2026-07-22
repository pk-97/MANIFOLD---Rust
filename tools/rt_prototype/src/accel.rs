//! Acceleration structure build + refit, BRIEF.md step 2.

use manifold_gpu::GpuBuffer;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSArray;
use objc2_metal::{
    MTLAccelerationStructure, MTLAccelerationStructureCommandEncoder,
    MTLAccelerationStructureGeometryDescriptor,
    MTLAccelerationStructureTriangleGeometryDescriptor, MTLAccelerationStructureUsage,
    MTLAttributeFormat, MTLCommandBuffer, MTLCommandEncoder, MTLDevice, MTLIndexType,
    MTLPrimitiveAccelerationStructureDescriptor,
};

use crate::gpu::Gpu;

pub struct Accel {
    pub structure: Retained<ProtocolObject<dyn MTLAccelerationStructure>>,
    pub descriptor: Retained<MTLPrimitiveAccelerationStructureDescriptor>,
    /// Kept alive for the refit scratch buffer's lifetime (the build
    /// scratch buffer is only read by the GPU during `build()`, which
    /// already commits+waits, so it does not need to outlive this call).
    pub refit_scratch: GpuBuffer,
}

fn make_descriptor(
    vertex_buffer: &GpuBuffer,
    index_buffer: &GpuBuffer,
    triangle_count: u32,
    usage: MTLAccelerationStructureUsage,
) -> Retained<MTLPrimitiveAccelerationStructureDescriptor> {
    let tri_desc = MTLAccelerationStructureTriangleGeometryDescriptor::descriptor();
    tri_desc.setVertexBuffer(Some(vertex_buffer.raw()));
    tri_desc.setVertexFormat(MTLAttributeFormat::Float3);
    tri_desc.setVertexStride(12);
    tri_desc.setIndexBuffer(Some(index_buffer.raw()));
    tri_desc.setIndexType(MTLIndexType::UInt32);
    tri_desc.setTriangleCount(triangle_count as usize);
    tri_desc.setOpaque(true);
    let geom: Retained<MTLAccelerationStructureGeometryDescriptor> = tri_desc.into_super();
    let array = NSArray::from_retained_slice(&[geom]);
    let prim_desc = MTLPrimitiveAccelerationStructureDescriptor::descriptor();
    prim_desc.setGeometryDescriptors(Some(&array));
    prim_desc.setUsage(usage);
    prim_desc
}

/// Build a fresh acceleration structure over `vertex_buffer` (packed_float3,
/// stride 12) / `index_buffer` (u32 triples). Returns the built `Accel` plus
/// CPU wall-clock and GPU command-buffer build time in ms.
pub fn build(gpu: &Gpu, vertex_buffer: &GpuBuffer, index_buffer: &GpuBuffer, triangle_count: u32) -> (Accel, f64, f64) {
    let descriptor = make_descriptor(vertex_buffer, index_buffer, triangle_count, MTLAccelerationStructureUsage::Refit);
    let raw_device = gpu.device.raw_device();
    let sizes = raw_device.accelerationStructureSizesWithDescriptor(&descriptor);
    let structure = raw_device
        .newAccelerationStructureWithSize(sizes.accelerationStructureSize)
        .expect("newAccelerationStructureWithSize failed");
    let scratch = gpu.buffer_zeroed(sizes.buildScratchBufferSize.max(16));
    let refit_scratch = gpu.buffer_zeroed(sizes.refitScratchBufferSize.max(16));

    let cpu_start = std::time::Instant::now();
    let cb = gpu.command_buffer("AS build");
    let enc = cb
        .accelerationStructureCommandEncoder()
        .expect("accelerationStructureCommandEncoder failed");
    enc.buildAccelerationStructure_descriptor_scratchBuffer_scratchBufferOffset(
        &structure, &descriptor, scratch.raw(), 0,
    );
    enc.endEncoding();
    let gpu_ms = Gpu::commit_and_time(&cb);
    let cpu_ms = cpu_start.elapsed().as_secs_f64() * 1000.0;

    (
        Accel { structure, descriptor, refit_scratch },
        cpu_ms,
        gpu_ms,
    )
}

/// Refit `accel` in place against the (already GPU-side-modified) vertex
/// buffer. Returns GPU command-buffer time in ms.
pub fn refit(gpu: &Gpu, accel: &Accel) -> f64 {
    let cb = gpu.command_buffer("AS refit");
    let enc = cb
        .accelerationStructureCommandEncoder()
        .expect("accelerationStructureCommandEncoder failed");
    unsafe {
        enc.refitAccelerationStructure_descriptor_destination_scratchBuffer_scratchBufferOffset(
            &accel.structure,
            &accel.descriptor,
            Some(&accel.structure),
            Some(accel.refit_scratch.raw()),
            0,
        );
    }
    enc.endEncoding();
    Gpu::commit_and_time(&cb)
}
