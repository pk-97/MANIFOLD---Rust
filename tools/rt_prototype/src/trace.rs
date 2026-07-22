//! Compute pass dispatch for trace_lighting / upsample_lighting /
//! shade_combine, per the binding tables in `shaders/rt_trace.metal`
//! (buffer(0)=accel, buffer(1)=TraceParams, buffer(2)=Material*,
//! buffer(3)=mat_index; textures 0..3 as documented per kernel).

use manifold_gpu::{GpuBuffer, GpuTexture};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLAccelerationStructure, MTLCommandBuffer, MTLCommandEncoder, MTLComputeCommandEncoder,
    MTLComputePipelineState, MTLLibrary, MTLResourceUsage, MTLSize,
};

use crate::gpu::Gpu;

pub struct TracePipelines {
    pub trace_lighting: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    pub upsample_lighting: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    pub shade_combine: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
}

impl TracePipelines {
    pub fn new(gpu: &Gpu, library: &ProtocolObject<dyn MTLLibrary>) -> Self {
        Self {
            trace_lighting: gpu.compute_pipeline(library, "trace_lighting"),
            upsample_lighting: gpu.compute_pipeline(library, "upsample_lighting"),
            shade_combine: gpu.compute_pipeline(library, "shade_combine"),
        }
    }
}

const TG: MTLSize = MTLSize { width: 8, height: 8, depth: 1 };

fn threadgroups_for(w: u32, h: u32) -> MTLSize {
    MTLSize { width: (w as usize).div_ceil(8), height: (h as usize).div_ceil(8), depth: 1 }
}

#[allow(clippy::too_many_arguments)]
pub fn dispatch_trace_lighting(
    gpu: &Gpu,
    pipeline: &ProtocolObject<dyn MTLComputePipelineState>,
    accel: &ProtocolObject<dyn MTLAccelerationStructure>,
    params: &GpuBuffer,
    materials: &GpuBuffer,
    mat_index: &GpuBuffer,
    g_wpos: &GpuTexture,
    g_nrm: &GpuTexture,
    out_sv: &GpuTexture,
    out_gi: &GpuTexture,
    trace_w: u32,
    trace_h: u32,
) -> f64 {
    let cb = gpu.command_buffer("trace_lighting");
    let enc = cb.computeCommandEncoder().expect("computeCommandEncoder failed");
    unsafe {
        enc.setComputePipelineState(pipeline);
        enc.useResource_usage(accel.as_ref(), MTLResourceUsage::Read);
        enc.setAccelerationStructure_atBufferIndex(Some(accel), 0);
        enc.setBuffer_offset_atIndex(Some(params.raw()), 0, 1);
        enc.setBuffer_offset_atIndex(Some(materials.raw()), 0, 2);
        enc.setBuffer_offset_atIndex(Some(mat_index.raw()), 0, 3);
        enc.setTexture_atIndex(Some(g_wpos.raw()), 0);
        enc.setTexture_atIndex(Some(g_nrm.raw()), 1);
        enc.setTexture_atIndex(Some(out_sv.raw()), 2);
        enc.setTexture_atIndex(Some(out_gi.raw()), 3);
        enc.dispatchThreadgroups_threadsPerThreadgroup(threadgroups_for(trace_w, trace_h), TG);
    }
    enc.endEncoding();
    Gpu::commit_and_time(&cb)
}

#[allow(clippy::too_many_arguments)]
pub fn dispatch_upsample_lighting(
    gpu: &Gpu,
    pipeline: &ProtocolObject<dyn MTLComputePipelineState>,
    params: &GpuBuffer,
    g_wpos: &GpuTexture,
    g_nrm: &GpuTexture,
    lo_sv: &GpuTexture,
    lo_gi: &GpuTexture,
    hi_sv: &GpuTexture,
    hi_gi: &GpuTexture,
    gbuffer_w: u32,
    gbuffer_h: u32,
) -> f64 {
    let cb = gpu.command_buffer("upsample_lighting");
    let enc = cb.computeCommandEncoder().expect("computeCommandEncoder failed");
    unsafe {
        enc.setComputePipelineState(pipeline);
        enc.setBuffer_offset_atIndex(Some(params.raw()), 0, 1);
        enc.setTexture_atIndex(Some(g_wpos.raw()), 0);
        enc.setTexture_atIndex(Some(g_nrm.raw()), 1);
        enc.setTexture_atIndex(Some(lo_sv.raw()), 2);
        enc.setTexture_atIndex(Some(lo_gi.raw()), 3);
        enc.setTexture_atIndex(Some(hi_sv.raw()), 4);
        enc.setTexture_atIndex(Some(hi_gi.raw()), 5);
        enc.dispatchThreadgroups_threadsPerThreadgroup(threadgroups_for(gbuffer_w, gbuffer_h), TG);
    }
    enc.endEncoding();
    Gpu::commit_and_time(&cb)
}

#[allow(clippy::too_many_arguments)]
pub fn dispatch_shade_combine(
    gpu: &Gpu,
    pipeline: &ProtocolObject<dyn MTLComputePipelineState>,
    params: &GpuBuffer,
    g_wpos: &GpuTexture,
    g_nrm: &GpuTexture,
    g_alb: &GpuTexture,
    g_mat: &GpuTexture,
    sv: &GpuTexture,
    gi: &GpuTexture,
    out_hdr: &GpuTexture,
    cam_pos: &GpuBuffer,
    gbuffer_w: u32,
    gbuffer_h: u32,
) -> f64 {
    let cb = gpu.command_buffer("shade_combine");
    let enc = cb.computeCommandEncoder().expect("computeCommandEncoder failed");
    unsafe {
        enc.setComputePipelineState(pipeline);
        enc.setBuffer_offset_atIndex(Some(params.raw()), 0, 1);
        enc.setBuffer_offset_atIndex(Some(cam_pos.raw()), 0, 2);
        enc.setTexture_atIndex(Some(g_wpos.raw()), 0);
        enc.setTexture_atIndex(Some(g_nrm.raw()), 1);
        enc.setTexture_atIndex(Some(g_alb.raw()), 2);
        enc.setTexture_atIndex(Some(g_mat.raw()), 3);
        enc.setTexture_atIndex(Some(sv.raw()), 4);
        enc.setTexture_atIndex(Some(gi.raw()), 5);
        enc.setTexture_atIndex(Some(out_hdr.raw()), 6);
        enc.dispatchThreadgroups_threadsPerThreadgroup(threadgroups_for(gbuffer_w, gbuffer_h), TG);
    }
    enc.endEncoding();
    Gpu::commit_and_time(&cb)
}
